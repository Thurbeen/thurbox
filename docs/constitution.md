# Constitution

Non-negotiable rules for everything in this repository: thurbox, Fleet and
the runtimes. Each rule has an automated enforcement mechanism, or names the
review that enforces it. A rule that nothing can enforce does not belong here.

The rules come in two parts:

- [Every component](#every-component): rules that hold for thurbox, Fleet and
  every runtime alike.
- [Per component](#per-component): rules that hold inside one component.

A component's rule may add to a shared rule and never relaxes one. Where a
mechanism exists in one component only, the enforcement map says so, and the
[migration](README.md#migration) brings it to the whole workspace.

## Every component

### 1. Components meet only through processes

thurbox, Fleet and each runtime call each other by running a program and
reading its answer. No Rust dependency crosses from one component to another.
The contracts are the [views](fleet/architecture.md#views), the
[runtime protocol](runtime/README.md#the-runtime-protocol), the
[terminal stream](runtime/README.md#the-terminal-stream) and the
[recipe contract](fleet/architecture.md#recipes).

A wire format is not a component: `term-stream` holds only frame types and
their codec, and any component may depend on it.

### 2. An unanswered question is Unknown

A question that got no proof is Unknown. It is never read as "no", "absent"
or "idle". Unknown never concludes anything and never triggers a destructive
effect. A later observation answers it.

Fleet carries this in its `Observation` type. thurbox's backend contract
carries it in `WindowLifecycle::discover`, where an empty list means the
backend proved it holds nothing and an unreachable host is an error. Every
runtime carries it in its answers.

### 3. Crash-free operation

Errors are displayed in the UI (status bar / footer), never via panics.
A panic is a bug. The panic hook installed by `coordinator::boot` restores
the terminal first, so the user's terminal stays usable if one happens.

Code review enforces this, not a lint: `clippy::unwrap_used` is not denied,
and start-up still calls `expect` on resources it cannot run without, such as
the log file in the data directory.

Fleet reports a failure as a command's error output and exit status, and a
controller's failure as a condition on the resource.

### 4. Zero-warning policy

Both `clippy` and `rustdoc` run with warnings promoted to errors.
`rumdl` enforces markdown style (100-char line width, consistent
formatting). If any linter reports warnings, CI fails.

### 5. Permissive licenses only

All dependencies must carry licenses from the allowlist in `deny.toml`.
Copyleft crates are rejected at PR time.

### 6. Zero known vulnerabilities

`cargo-deny` advisories blocks merges
when known CVEs affect the dependency tree.

### 7. Conventional commits

Every commit that reaches `main` is validated against the Conventional Commits
spec by `cocogitto`. Pull requests land by squash merge, so that commit is the
pull request **title**, and the required `PR Title` check
(`scripts/ci/check-pr-title.sh`) is what rejects a non-conforming one. The
`commit-msg` hook holds a branch's own commits to the same spec locally, for a
legible history; squash discards them, so nothing in CI checks them.

### 8. Test-driven development (Red, Green, Refactor)

All features and bug fixes follow the TDD/BDD cycle:

1. **Red** — Write a failing test that defines the expected behavior.
2. **Green** — Write the minimum code to make the test pass.
3. **Refactor** — Clean up while keeping tests green.

Tests are written *before* or *alongside* the implementation,
never as an afterthought. If a bug is reported,
the fix starts with a test that reproduces it.

A change to a contract starts with an end-to-end test across the components
it joins.

### 9. Deterministic CI: scripts over LLMs

CI pipelines must be reproducible and deterministic. Every check is a
script or tool that produces the same result given the same input.
LLM-generated judgments (code review bots, AI-powered linters)
are never gating — they may advise, but deterministic tools
(`clippy`, `nextest`, `cargo-deny`, `cog`, `rumdl`, `shellcheck`)
make the pass/fail decision.
Changes to CI configuration require careful review
because a broken pipeline affects every contributor.

A gate is never weakened to make a change pass.

### 10. Tag-based versioning

Version numbers are determined by git tags, not Cargo.toml.
The release workflow analyzes conventional commits, creates tags automatically,
and builds binaries with versions injected at build time.
No version bump commits pollute the git history.

**Why:** Automated version commits add noise without value. Tags are the
natural place for release markers. Build-time version detection ensures
binaries have correct versions while keeping the source tree clean.

**Mechanism:**

1. Release workflow (`cd.yml`) analyzes commits via `cog bump --auto --dry-run`
2. Workflow creates lightweight tag (v{version}) and passes version via environment variable
3. `build.rs` reads `THURBOX_RELEASE_VERSION` and injects into binary
4. Cargo.toml version remains `0.0.0-dev` (development marker only)

**Result:**

- Release builds: version from tag (e.g., 1.0.0)
- Development builds: version from Cargo.toml (0.0.0-dev)

Each component is versioned by its own tag (`thurbox-v*`, `fleet-v*`,
`runtime-mux-v*`) once they share this repository; until then each
repository's `v*` tags apply. Fleet's `Cargo.toml` likewise stays
`0.0.0-dev`.

### 11. Credentials stay with the agent and the host

No component reads, stores or forwards a coding agent's credentials. Fleet
sends none to a runtime or a host; thurbox reads no agent account; a runtime
uses the host's own.

## Per component

### thurbox

#### T1. Module isolation

Domain dependency flow is one-directional. Every module under `src/` has
exactly one line here, and each line is the rule `tests/architecture_rules.rs`
asserts for it (`[path-only: …]` marks the crossings described below):

```text
session              (no crate-internal references — the dependency sink;
                     no process state, env read or rendering crate either)
agent                → agent::{generic, provider}       (re-exports only)
agent::agent_config  → session, shell, config::file
agent::extension_config → session, paths, agent::agent_config, config::file
agent::generic       → session, agent::provider
agent::host_path     → session, shell
agent::input         (no crate-internal references)
agent::json_merge    (no crate-internal references)
agent::preflight     → session, paths, shell, agent::agent_config
agent::provider      → session
agent::toml_merge    (no crate-internal references)
config               (declares the modules below — no references)
config::file         → paths                (the one loader: seed, parse, save)
config::hooks        → session, config::file
config::hosts        → session, paths, shell, config::file
config::settings     → session, config::file
config::themes       → theme, config::file
update               (declares the modules below — no references)
update::self_update  → session, paths, shell, agent::extension_config,
                       update::version_check
update::version_check → session, paths, agent::extension_config
backend              → backend::{contract, pane, registry}   (re-exports only)
backend::contract    (no crate-internal references)
backend::identity    → backend::contract
backend::instance    → session, paths                 (ADR-12 socket naming)
backend::pane        → session, backend::{contract, identity, osc8, output_wake}
backend::osc8        → session
backend::output_wake (no crate-internal references)
backend::registry    → session, backend::contract
backend::wiring      → session, shell, config::hosts,
                       backend::{contract, registry, tmux, psmux, rmux}
backend::tmux_compat (declares the modules below — no references)
  ::control_mode     → shell, backend::contract,
                       backend::tmux_compat::transport
  ::server           → session, paths, shell, agent::{host_path, preflight},
                       backend::{contract, identity, instance},
                       backend::tmux_compat::{control_mode, transport}
  ::transport        → shell, agent::preflight
backend::tmux        → session, shell, backend::contract,
                       backend::tmux_compat::{control_mode, server, transport}
backend::psmux       → session, shell, backend::{contract, instance},
                       backend::tmux_compat::{control_mode, server, transport}
backend::rmux        → session, shell, backend::contract,
                       backend::tmux_compat::{control_mode, server, transport}
git                  → session, paths, shell
storage              → session, paths
telemetry            → session, storage           (ADR-37/38 OTLP export)
session_ops          → session, storage, git, paths, workspace, shell
                       [path-only: agent::{agent_config, extension_config,
                        generic, host_path, json_merge, preflight, provider,
                        toml_merge}, backend::{contract, identity, instance,
                        registry}, config::{hooks, hosts, settings},
                        update::{self_update, version_check}]
kernel               → session, storage, paths, session_ops, git,
                       notifications, shell, theme
                       [path-only: agent::{agent_config, extension_config,
                        host_path, preflight}, backend::{contract, identity,
                        pane, registry}, config::{hosts, settings, themes},
                        update::{self_update, version_check}]
cli                  → session, storage, session_ops, paths,
                       notifications, ui_control, telemetry, theme
                       [path-only: agent::{agent_config, extension_config,
                        preflight}, backend::{contract, instance, registry},
                        config::{file, hooks, hosts, settings, themes},
                        update::{self_update, version_check}, kernel]
notifications        → session, paths, shell    [path-only: storage]
clipboard            → session, paths
workspace            → paths
paths                (leaf utility — no crate-internal references)
ui_control           → paths
shell                → session                 (a host entry → its launcher,
                                                the local platform)
theme                (no crate-internal references — the presets)
coordinator          → agent::input, backend::{instance, output_wake, wiring},
                       clipboard, config::settings, kernel, paths, session,
                       session_ops, shell, storage, telemetry, ui_control
```

`agent` holds coding-agent definitions and never touches `git`, `storage`,
`kernel` or `backend`; `config` holds thurbox's own config files, each read
and written through the one loader `config::file`, and `update` the
self-update; `session` holds plain data and
references nothing, which is what lets every other module depend on it.
`backend` is the session-backend boundary: consumers name its contract
(`backend::contract`, `backend::identity`, `backend::pane`,
`backend::registry`), and only the factory, `backend::wiring`, names an
adapter. Only a composition root may reach the factory. The tmux, psmux, and
RMUX adapters are peers: none reaches another, and `backend::tmux_compat`, the
protocol they speak, reaches none (ADR-31). `session_ops`, `cli` and
`kernel` reach no adapter, protocol helper or factory at all — not by
reference, alias or re-export, and not through anything they are granted —
status and the heartbeat included (ADR-32). Every file of `agent`, `config`
and `update` is a node of its own, so a grant names the file it reads.

Some crossings are permitted **by fully-qualified path only**, never by
`use` — not even a function-local `use` or an alias. The restriction is
the point — every crossing into the side-effect layer stays visible at
its call site instead of disappearing into an import list.

`coordinator` is `main`'s own body split across files — the loop, the
workers and the chrome. It wires the layers together, so its list is
the widest, but it **is** a list: a new layer reached from the loop is
a decision recorded in the test, not an exemption. Only the crate roots
(`bin`, `lib`, `main`) are exempt.

`tests/architecture_rules.rs` is an **allowlist** and checks every entry
in one loop: a new `src/` module fails until its dependencies are
declared there, and a declared rule cannot go unasserted. It judges a
reference by what it **resolves to** — through `super::`, brace groups,
re-exports and `type` aliases — so an alias crosses nothing a path could
not. The graph of actual edges and the graph the rules declare must both
be acyclic, and a grant nothing uses fails. A crossing that breaks a rule
today and is scheduled to go is listed item by item, with the task that
removes it, in the test's `TRANSITIONAL` table; the table must equal the
crossings the source makes, and it ends empty.

#### T2. The interface is a plugin kernel, and its five rules hold

The interface is Lua on a Rust kernel (ADR-23). v1's TEA loop — a single
`App` model with `update()`/`view()` — was the sanctioned pattern until
`src/app` and `src/ui` were deleted; what replaced it is five rules, each
with a mechanism rather than a review habit:

1. **Four node kinds, forever** — `text`, `box`, `input`, `surface`.
   Everything else composes in Lua: `ui/lib/ui.lua` (the component
   layer) over `ui/lib/widgets.lua` (the primitive kit).
   *Enforced by* `tests/kernel_mvp.rs`, which asserts the count.
2. **Layout resolves before render** — rects are computed first, then
   each plugin is called with its own. Plugins declare size statically,
   which is what breaks the circularity.
3. **Snapshot-read, command-write** — reads come from an in-memory
   snapshot and return instantly; writes are commands accepted now and
   surfaced later. Lua never blocks the loop on SQLite, git or an
   unreachable host.
   *Enforced by* mlua's `send` feature being deliberately left off, which
   makes "plugins never touch the render thread" a compile error.
4. **Capabilities by absence** — an ungranted capability is *not in the
   environment*; `io`, `os`, `debug`, `package` and the loaders are
   withheld.
   *Enforced by* `thurbox.yml` (selene's stdlib for `ui/`, which declares
   no `base:`), `lua-language-server`'s `runtime.builtin`, and
   `tests/kernel_mvp.rs`, which enumerates the plugin environment
   global-by-global so a new one has to be added deliberately.
5. **Anything touching the world runs on a worker** — terminal attach,
   commands, the interface's database reads and writes, diffs, metrics, git
   stats, repository reads, update checks, and programs a plugin asked for.

No ad-hoc event handlers, no component-local state, no callback chains.

#### T3. Backend-first session model

Coding-agent sessions run via a `SessionBackend` trait (four roles —
`PaneStream`, `WindowLifecycle`, `PaneIo`, `StatusChannel` — plus a
`Capabilities` value), one backend per route
in the registry (ADR-29). The default is the platform's multiplexer run
locally (`tmux -L thurbox`; `psmux` on native Windows). Each adapter —
`TmuxBackend`, `PsmuxBackend`, `RmuxBackend`, peers over one tmux-protocol server (ADR-31) —
runs over a transport, local, SSH or WSL, so a session can live on another
host with no adapter of its own for that (ADR-13). The multiplexer provides
truly persistent sessions that survive crashes/restarts.
We never mock, emulate, or screen-scrape a fake terminal.

The multiplexer is the source of truth for what is running: a pane, its
process and its output. The database is the source of truth for which sessions
should exist. A lifecycle change records its intent in a row before it touches
a pane or a worktree, and self-heal, the sweeps and the repairs read the rows
to decide what to recreate, reap or repair (ADR-26). A host's own database owns
its sessions; another instance is a client of it (ADR-24).

This rule holds until [ADR-41](thurbox/architecture.md#adr-41-fleet-is-thurboxs-engine-sessions-run-on-runtimes)
lands: sessions then belong to Fleet and run on a runtime, and thurbox keeps
no session rows.

#### T4. Logging never touches stdout

Stdout belongs to the TUI. All diagnostic output goes to the log file
at `~/.local/share/thurbox/thurbox.log`.

### Fleet

#### F1. Decisions are pure; adapters do the I/O

`fleet-api` and `fleet-controllers` are pure: a controller receives time and
observations as values and returns probes and a plan. Adapters (store,
runtime, forge, gateway, plugin) perform every I/O through a port. The manager
persists plans under the lease; the CLI parses, wires and prints.
`crates/architecture.toml` declares every crate's allowed dependencies, and
`fleet-arch` checks crate edges and purity, with positive and negative
fixtures.

#### F2. One process spawner, always bounded

`fleet-exec` is the only production process spawner. Every call has a bound,
and timeout or unwind kills the whole process tree. `fleet-arch` checks the
restricted process APIs.

#### F3. An effect has an intent and a receipt

The manager persists a deterministic intent before an external effect and a
receipt after it. A missing receipt is recovered by observing the effect's
key, never by repeating the effect blindly. The contract tests named in
[contracts.md](fleet/contracts.md) hold every effect to this.

#### F4. One writer per status field

Each status field has one owner, declared in `crates/architecture.toml` and
checked by `fleet-arch`. The inbox is the one typed exception, and every
write to it is a typed store mutation.

### Runtimes

#### R1. A runtime knows no Task

A runtime runs keyed processes and workspace recipes at its places. It knows
no Task, harness, journal or delivery. The protocol carries none of them.

#### R2. Idempotent by key, and Unknown is not None

Every verb is idempotent by key: two starts with one key leave one process,
and a stop of an absent process succeeds. An error answers Unknown, never
None. `fleet runtime check` holds every runtime to this, and a recipe is held
to the same by `fleet recipe check`.

## Enforcement map

| Rule | Enforced by | Where |
|---|---|---|
| 1. Components meet only through processes | One `architecture.toml` checked by `fleet-arch` | From migration step 3; today the components are separate repositories |
| 2. Unknown never concludes | Fleet's `Observation` type and contract tests; thurbox's `discover` contract; `fleet runtime check` | Fleet, thurbox; runtimes from step 4 |
| 3. Crash-free operation | Code review | All |
| 4. Zero warnings | `clippy -D warnings` + `RUSTDOCFLAGS="-D warnings"` + `rumdl` | thurbox CI + pre-commit; Fleet `cargo xtask check` |
| 5. Permissive licenses | `cargo-deny check bans licenses` | `deny.toml` in each |
| 6. Zero vulnerabilities | `cargo-deny check advisories` | `deny.toml` in each |
| 7. Conventional commits | `scripts/ci/check-pr-title.sh` (required `PR Title` check) + `cog verify` in `commit-msg` | `cog.toml` in each |
| 8. Test first | `cargo-nextest` + code review | `.config/nextest.toml` |
| 9. Deterministic CI | Scripts and tools only; no LLM-gated checks | CI config + pre-commit |
| 10. Tag-based versioning | `build.rs` + `cd.yml` (thurbox); `cog bump` in `cd.yml` (Fleet) | `.github/workflows/cd.yml` |
| 11. Credentials stay put | Code review | All |
| T1. Module isolation | `tests/architecture_rules.rs` | thurbox |
| T2. Plugin-kernel rules | `tests/kernel_mvp.rs` + `thurbox.yml` (selene) + `.luarc.json` (luals) | thurbox |
| T3. Backend-first model | Code review | thurbox |
| T4. Logging off stdout | Code review | thurbox |
| F1. Pure decisions | `fleet-arch` + `crates/architecture.toml` | Fleet |
| F2. One bounded spawner | `fleet-arch` (restricted process APIs) | Fleet |
| F3. Intent and receipt | Contract tests in `fleet/contracts.md` | Fleet |
| F4. One writer per field | `fleet-arch` (permitted status writers) | Fleet |
| R1. A runtime knows no Task | The protocol's schema | From step 4 |
| R2. Idempotent by key | `fleet runtime check`, `fleet recipe check` | From step 4 |
