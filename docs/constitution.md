# Constitution

Non-negotiable rules for everything in this repository: thurbox, Fleet and
the runtimes. Each rule has an automated enforcement mechanism, or names the
review that enforces it. A rule that nothing can enforce does not belong here.

The rules come in two parts:

- [Every component](#every-component): rules that hold for thurbox, Fleet and
  every runtime alike.
- [Per component](#per-component): rules that hold inside one component.

A component's rule may add to a shared rule and never relaxes one. Where a
mechanism exists in one component only, the enforcement map says so.

## Every component

### 1. Components meet only through processes

thurbox, Fleet and each runtime call each other by running a program and
reading its answer. No Rust dependency crosses from one component to another.
The contracts are the [views](fleet/architecture.md#views), the
[runtime protocol](runtime/README.md#the-runtime-protocol), the
[terminal stream](runtime/README.md#the-terminal-stream) and
[workspaces](runtime/README.md#workspaces).

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

Domain dependency flow is one-directional. thurbox's module rules in
`crates/architecture.toml` hold the rule: every module under `src/` has one
entry there naming what it may reference, and that file is the one copy of
the graph.

`agent` holds downloads and keyboard input and never touches `git`, `kernel`
or `backend`; `config` holds thurbox's own config files, each read
and written through the one loader `config::file`, and `update` the
self-update; `session` holds plain data and
references nothing, which is what lets every other module depend on it.
`backend` is the session-backend boundary as thurbox reaches it: the
multiplexer runtime, `runtime-mux`, re-exported, with the settings and data
directory thurbox injects into it. Consumers name its contract
(`backend::contract`, `backend::identity`, `backend::pane`,
`backend::registry`), and none names an adapter (`backend::tmux`, `psmux`,
`rmux`), the protocol they speak (`backend::tmux_compat`) or the factory
(`backend::wiring`); only the composition root, `coordinator`, reaches the
factory. `cli` and `kernel` reach none of those at all — not
by reference, alias or re-export. Every file of `agent`, `config` and `update` is a node of its own,
so a grant names the file it reads.

The runtime's own modules are held to the same kind of allowlist by
their module rules in `crates/architecture.toml`, checked by `arch-rules` and
listed under
[R3](#r3-runtime-mux-module-isolation) below: the tmux, psmux and RMUX adapters are
peers, none reaches another, `tmux_compat`, the protocol they speak, reaches
none (ADR-31), and only the factory names one.

Some crossings are permitted **by fully-qualified path only**, never by
`use` — not even a function-local `use` or an alias. The restriction is
the point — every crossing into the side-effect layer stays visible at
its call site instead of disappearing into an import list.

`coordinator` is `main`'s own body split across files — the loop, the
workers and the chrome. It wires the layers together, so its list is
the widest, but it **is** a list: a new layer reached from the loop is
a decision recorded in the test, not an exemption. Only the crate roots
(`bin`, `lib`, `main`) are exempt.

`arch-rules` checks that **allowlist** entry by entry: a new `src/` module
fails until its dependencies are declared there, and a declared rule cannot
go unasserted. It judges a
reference by what it **resolves to** — through `super::`, brace groups,
re-exports and `type` aliases — so an alias crosses nothing a path could
not. The graph of actual edges and the graph the rules declare must both
be acyclic, and a grant nothing uses fails.

#### T2. The interface is a plugin kernel, and its five rules hold

The interface is Lua on a Rust kernel (ADR-23). Five rules carry it, each
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
   surfaced later. Lua never blocks the loop on Fleet, git or an
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
   commands, the `fleet watch` that feeds the snapshot, diffs, metrics, git
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
host with no adapter of its own for that
([runtime-mux](runtime/runtime-mux.md#hosts-and-transports)). The multiplexer provides
truly persistent sessions that survive crashes/restarts.
We never mock, emulate, or screen-scrape a fake terminal.

The multiplexer is the source of truth for what is running: a pane, its
process and its output. Fleet's store is the source of truth for which
sessions exist ([ADR-41](thurbox/architecture.md#adr-41-fleet-is-thurboxs-engine-sessions-run-on-runtimes)):
every session is a Fleet Task, and thurbox keeps no session rows.

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
`arch-rules` checks crate edges and purity, with positive and negative
fixtures.

#### F2. One process spawner, always bounded

`fleet-exec` is the only production process spawner. Every call has a bound,
and timeout or unwind kills the whole process tree. `arch-rules` checks the
restricted process APIs.

#### F3. An effect has an intent and a receipt

The manager persists a deterministic intent before an external effect and a
receipt after it. A missing receipt is recovered by observing the effect's
key, never by repeating the effect blindly. The contract tests named in
[contracts.md](fleet/contracts.md) hold every effect to this.

#### F4. One writer per status field

Each status field has one owner, declared in `crates/architecture.toml` and
checked by `arch-rules`. The exceptions are the shared writes
[contracts.md](fleet/contracts.md) lists: the inbox, `status.answered`, a
Conversation's `status.received` and a Listener's beat fields. Every write to
one of them is a typed store mutation.

### Runtimes

#### R1. A runtime knows no Task

A runtime runs keyed processes and makes their workspaces at its places. It knows
no Task, harness, journal or delivery. The protocol carries none of them.

#### R2. Idempotent by key, and Unknown is not None

Every verb is idempotent by key: two starts with one key leave one process,
and a stop of an absent process succeeds. An error answers Unknown, never
None. A release never removes what the runtime did not make and never loses
work. `fleet runtime check` holds every runtime to this.

#### R3. runtime-mux module isolation

`runtime-mux` depends on no other crate of the repository, and reads nothing
of the program embedding it: the settings it honours and where that program
keeps its data are passed in (`MuxSettings`, `HostsConfig`). Inside it, the
module rules in `crates/architecture.toml` give every module under
`crates/runtime-mux/src/` one entry naming what it may reference, and
`arch-rules` checks them; that file is the one copy of the graph.
`crates/runtime-mux/tests/architecture_rules.rs` checks the boundary's shape
over those rules.

Only the factory, `wiring`, names an adapter, and no module of the runtime
reaches the factory: the `runtime-mux` program's `main.rs` calls it, as an
embedder's composition root does. A specific multiplexer is named only by
`host` (the route grammar and its defaults), `preflight`, the factory and the
one adapter that is it. `host/route.rs` and `contract.rs` reach no launcher, adapter or
build OS ([a host's platform](runtime/runtime-mux.md#a-hosts-platform)).

## Enforcement map

| Rule | Enforced by | Where |
|---|---|---|
| 1. Components meet only through processes | `crates/architecture.toml` checked by `arch-rules`: every other component is `role = "component"`, held to the crate graph from its manifest and its sources | The workspace; thurbox links `runtime-mux` until Fleet drives the runtimes |
| 2. Unknown never concludes | Fleet's `Observation` type and contract tests; thurbox's `discover` contract; `fleet runtime check` | Fleet, thurbox, runtime-mux, `examples/runtimes/tmux.sh` |
| 3. Crash-free operation | Code review | All |
| 4. Zero warnings | `clippy -D warnings` + `RUSTDOCFLAGS="-D warnings"` + `rumdl` | CI + pre-commit, over every crate |
| 5. Permissive licenses | `cargo-deny check bans licenses` | `deny.toml` |
| 6. Zero vulnerabilities | `cargo-deny check advisories` | `deny.toml` |
| 7. Conventional commits | `scripts/ci/check-pr-title.sh` (required `PR Title` check) + `cog verify` in `commit-msg` | `cog.toml` |
| 8. Test first | `cargo-nextest` + code review | `.config/nextest.toml` |
| 9. Deterministic CI | Scripts and tools only; no LLM-gated checks | CI config + pre-commit |
| 10. Tag-based versioning | `build.rs` + `cd.yml` (thurbox); `cog bump` in `cd.yml` (Fleet) | `.github/workflows/cd.yml` |
| 11. Credentials stay put | Code review | All |
| T1. Module isolation | `crates/architecture.toml` checked by `arch-rules`, + `crates/thurbox/tests/architecture_rules.rs` | thurbox |
| T2. Plugin-kernel rules | `tests/kernel_mvp.rs` + `thurbox.yml` (selene) + `.luarc.json` (luals) | thurbox |
| T3. Backend-first model | Code review | thurbox |
| T4. Logging off stdout | Code review | thurbox |
| F1. Pure decisions | `arch-rules` + `crates/architecture.toml` | Fleet |
| F2. One bounded spawner | `arch-rules` (restricted process APIs) | Fleet |
| F3. Intent and receipt | Contract tests in `fleet/contracts.md` | Fleet |
| F4. One writer per field | `arch-rules` (permitted status writers) | Fleet |
| R1. A runtime knows no Task | The protocol's schema | Every runtime |
| R2. Idempotent by key | `fleet runtime check`, run by `crates/fleet-cli/tests/e2e_runtime_exec.rs` | runtime-mux, `examples/runtimes/tmux.sh` |
| R3. runtime-mux module isolation | `crates/architecture.toml` checked by `arch-rules`, + `crates/runtime-mux/tests/architecture_rules.rs` | runtime-mux |
