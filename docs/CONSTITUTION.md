# Constitution

Non-negotiable rules that define what Thurbox **must** always be.
Each principle has an automated enforcement mechanism
— if it can't be enforced, it doesn't belong here.

## Principles

### 1. Crash-free operation

Errors are displayed in the UI (status bar / footer), never via panics.
The only panic path is the emergency terminal-restore hook in `main.rs`,
which exists to leave the user's terminal in a usable state
if something truly unexpected happens.

### 2. Module isolation

Domain dependency flow is one-directional. Every module under `src/` has
exactly one line here, and each line is the rule `tests/architecture_rules.rs`
asserts for it (`[path-only: …]` marks the crossings described below):

```text
session              (no crate-internal references — the dependency sink)
agent                → session, paths, shell
agent::host_config   → session, paths, agent
backend              → backend::{contract, pane, registry}   (re-exports only)
backend::contract    (no crate-internal references)
backend::identity    → backend::contract
backend::pane        → session, backend::{contract, identity, osc8, output_wake}
backend::osc8        → session
backend::output_wake (no crate-internal references)
backend::registry    → session, backend::contract
backend::wiring      → session, agent::host_config,
                       backend::{contract, registry, tmux}
backend::tmux_compat (declares the two below — no references)
  ::control_mode     → session, shell, backend::contract,
                       backend::tmux_compat::transport
  ::transport        → shell, agent
backend::tmux        → session, paths, shell, agent,
                       backend::{contract, identity},
                       backend::tmux_compat::{control_mode, transport}
git                  → session, paths, shell
storage              → session, sync, paths
sync                 → session
usage                → session, shell           [path-only: paths]
session_ops          → session, storage, git, sync, paths, workspace, shell
                       [path-only: agent, agent::host_config,
                        backend::{contract, identity}]
kernel               → session, storage, sync, paths, session_ops, git,
                       notifications, shell
                       [path-only: agent, agent::host_config,
                        backend::{contract, identity, pane, registry}, usage]
cli                  → session, storage, session_ops, sync, paths,
                       notifications
                       [path-only: agent, agent::host_config,
                        backend::contract, kernel]
notifications        → session, paths, shell    [path-only: storage]
clipboard            → session, paths
workspace            → paths
paths, shell         (leaf utilities — no crate-internal references)
coordinator          → agent, backend::output_wake, clipboard, kernel, paths,
                       session, session_ops, shell, storage
```

`agent` holds coding-agent definitions and their config and never touches
`git`, `storage`, `kernel` or `backend`; `session` holds plain data and
references nothing, which is what lets every other module depend on it.
`backend` is the session-backend boundary: consumers name its contract
(`backend::contract`, `backend::identity`, `backend::pane`,
`backend::registry`), and only the factory, `backend::wiring`, names an
adapter. Only a composition root may reach the factory.

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

### 3. Zero-warning policy

Both `clippy` and `rustdoc` run with warnings promoted to errors.
`rumdl` enforces markdown style (100-char line width, consistent
formatting). If any linter reports warnings, CI fails.

### 4. Permissive licenses only

All dependencies must carry licenses from the allowlist in `deny.toml`.
Copyleft crates are rejected at PR time.

### 5. Zero known vulnerabilities

`cargo-deny` advisories blocks merges
when known CVEs affect the dependency tree.

### 6. Conventional commits

Every commit that reaches `main` is validated against the Conventional Commits
spec by `cocogitto`. Pull requests land by squash merge, so that commit is the
pull request **title**, and the required `PR Title` check
(`scripts/ci/check-pr-title.sh`) is what rejects a non-conforming one. The
`commit-msg` hook holds a branch's own commits to the same spec locally, for a
legible history; squash discards them, so nothing in CI checks them.

### 7. The interface is a plugin kernel, and its five rules hold

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
   commands, diffs, metrics, git stats, repository reads, update checks,
   and programs a plugin asked for.

No ad-hoc event handlers, no component-local state, no callback chains.

### 8. Backend-first session model

Coding-agent sessions run via a `SessionBackend` trait. The default is
a local multiplexer (`tmux -L thurbox`; `psmux` on native Windows), and
the same `TmuxBackend` runs over a transport — local, SSH, or WSL — so a
session can live on another host without a second backend (ADR-13). The
multiplexer provides truly persistent sessions that survive
crashes/restarts.
We never mock, emulate, or screen-scrape a fake terminal.
The backend is the source of truth for session lifecycle.

### 9. Logging never touches stdout

Stdout belongs to the TUI. All diagnostic output goes to the log file
at `~/.local/share/thurbox/thurbox.log`.

### 10. Test-driven development (Red, Green, Refactor)

All features and bug fixes follow the TDD/BDD cycle:

1. **Red** — Write a failing test that defines the expected behavior.
2. **Green** — Write the minimum code to make the test pass.
3. **Refactor** — Clean up while keeping tests green.

Tests are written *before* or *alongside* the implementation,
never as an afterthought. If a bug is reported,
the fix starts with a test that reproduces it.

### 11. Deterministic CI — scripts over LLMs

CI pipelines must be reproducible and deterministic. Every check is a
script or tool that produces the same result given the same input.
LLM-generated judgments (code review bots, AI-powered linters)
are never gating — they may advise, but deterministic tools
(`clippy`, `nextest`, `cargo-deny`, `cog`, `rumdl`, `shellcheck`)
make the pass/fail decision.
Changes to CI configuration require careful review
because a broken pipeline affects every contributor.

### 12. Tag-based versioning

Version numbers are determined by git tags, not Cargo.toml.
The release workflow analyzes conventional commits, creates tags automatically,
and builds binaries with versions injected at build time.
No version bump commits pollute the git history.

**Why:** Automated version commits add noise without value. Tags are the
natural place for release markers. Build-time version detection ensures
binaries have correct versions while keeping the source tree clean.

**Mechanism:**

1. Release workflow (`release.yml`) analyzes commits via `cog bump --auto --dry-run`
2. Workflow creates lightweight tag (v{version}) and passes version via environment variable
3. `build.rs` reads `THURBOX_RELEASE_VERSION` and injects into binary
4. Cargo.toml version remains `0.0.0-dev` (development marker only)

**Result:**

- Release builds: version from tag (e.g., 1.0.0)
- Development builds: version from Cargo.toml (0.0.0-dev)

## Enforcement Map

| Principle | Enforced by | Config file |
|---|---|---|
| Crash-free operation | Code review + `#[deny(clippy::unwrap_used)]` (planned) | `clippy.toml` |
| Module isolation | `tests/architecture_rules.rs` | — |
| Zero warnings | `clippy -D warnings` + `RUSTDOCFLAGS="-D warnings"` + `rumdl` | CI + pre-commit |
| Permissive licenses | `cargo-deny check bans licenses` | `deny.toml` |
| Zero vulnerabilities | `cargo-deny check advisories` | `deny.toml` |
| Conventional commits | `scripts/ci/check-pr-title.sh` (required `PR Title` check) + `cog verify` in `commit-msg` | `cog.toml` |
| Plugin-kernel rules | `tests/kernel_mvp.rs` + `thurbox.yml` (selene) + `.luarc.json` (luals) | `thurbox.yml` |
| Backend-first model | Code review | — |
| Logging off stdout | Code review | — |
| TDD (Red/Green/Refactor) | `cargo-nextest` + code review | `.config/nextest.toml` |
| Deterministic CI | Scripts and tools only; no LLM-gated checks | CI config + pre-commit |
| Tag-based versioning | `build.rs` + `release.yml` | `build.rs` + `.github/workflows/release.yml` |
