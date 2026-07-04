# Contributing to Thurbox

Thanks for your interest in contributing to Thurbox! This guide covers how to
set up your environment, the conventions we follow, and how to get a change
merged.

Thurbox is a multi-session coding-agent TUI orchestrator built with Rust. Before
diving in, skimming the [`README.md`](README.md), [`CLAUDE.md`](CLAUDE.md), and
the design docs under [`docs/`](docs/) will save you time.

## Code of conduct

Be respectful, constructive, and welcoming. We want Thurbox to be a project
people enjoy contributing to — assume good faith, keep discussions on the
technical merits, and help newcomers find their footing.

## Getting started

1. **Clone** the repository — you can push branches directly, no fork needed.
2. **Create a branch** off `main` for your work
   (`git switch -c feat/my-change`).
3. **Set up the toolchain** (below).
4. **Make your change**, with tests.
5. **Run the checks** locally (`just lint && just test`).
6. **Push your branch** and **open a pull request** with a clear description.

## Development environment

The reproducible dev environment is a **Nix flake** that pins the Rust
toolchain, `tmux`, `shellcheck`, Node, the cargo tooling, `just`, and the demo
stack.

```bash
nix develop          # enter the pinned shell
# ...or, with direnv installed:
direnv allow         # auto-enters the shell on cd (see .envrc)
```

No Nix? Use the fallback installer:

```bash
scripts/install-dev-tools.sh   # installs the dev tools (including prek)
```

You'll also need `tmux >= 3.2`, `shellcheck`, `bats`, Node + npm (for the
website linters), and `git`. The full walkthrough — including the runtime
sandbox for trying thurbox in isolation — lives in
[`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

- **MSRV:** Rust 1.75, Edition 2021.

## Everyday tasks (`just`)

`just` is the task entrypoint — run `just` with no arguments for the full list.

| Task | What it does |
|------|--------------|
| `just build` | build the dev binaries (`thurbox` + `thurbox-cli`) |
| `just test` | `cargo nextest run --all` |
| `just lint` | fmt-check + clippy + cargo-deny + rumdl + shellcheck |
| `just fmt` | format Rust + website |
| `just arch` | architecture-rule + rustdoc checks |
| `just sandbox` | run thurbox in an isolated dev sandbox |

## Testing

Thurbox follows **test-driven development** — write a failing test first, make
it pass, then refactor. Bug fixes start with a test that reproduces the bug.

```bash
cargo nextest run --all              # run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # run a single test by name
```

The TUI has in-process acceptance tests with `insta` snapshots
(`src/app/acceptance.rs`) and a black-box smoke test
(`scripts/dev/smoke/tui-smoke.sh`). Update snapshots with
`INSTA_UPDATE=always cargo test` (or `cargo insta review`). See the Testing
section of [`CLAUDE.md`](CLAUDE.md) for the full picture.

## Linting & formatting

CI runs a **zero-warning policy** — `clippy` and `rustdoc` warnings are promoted
to errors. Run these before pushing:

```bash
cargo fmt --all                                            # format (100-char width)
cargo clippy --all-targets --all-features -- -D warnings   # lint
rumdl check .                                              # markdown lint
npm run lint:website                                       # website linters
```

`just lint` bundles the Rust + shell checks.

## Pre-commit hooks

We use [`prek`](https://github.com/j178/prek) (a Rust-based pre-commit
framework) to run the same checks CI does, automatically, before each commit.
**Install the hooks once after cloning:**

```bash
prek install
```

This is the recommended way to catch failures early — the hooks run across three
stages:

- **commit-msg** — conventional-commit validation (`cog verify`)
- **pre-commit** — fmt, clippy, check, nextest, architecture rules, cargo-deny,
  rustdoc, bats, shellcheck, rumdl, prettier, htmlhint, stylelint, eslint
- **pre-push** — commit-history check (`cog check`)

If a hook fails, fix the reported issue and re-stage — the same checks gate your
PR in CI, so a clean local run means a clean pipeline.

## Commit conventions

All commits **must** follow
[Conventional Commits](https://www.conventionalcommits.org/), enforced by
`cocogitto` via the `commit-msg` hook.

- **Types:** `feat`, `fix`, `perf`, `refactor`, `docs`, `style`, `test`,
  `chore`, `ci`, `build`, `revert`
- **Scopes:** `api`, `cli`, `ui`, `git`, `core`, `docs`, `deps`, `config`,
  `mcp`

```bash
cog commit feat "add remote host picker"
cog commit fix "avoid panic on empty worktree" git
```

Note that commit type drives releases: `feat` → minor bump, `fix`/`perf` →
patch bump, while `docs`/`chore`/`ci`/`style`/`test` produce no release.

## Documentation

If a change invalidates or extends a documented decision, update the relevant
doc in the **same PR**. Rationale lives in:

- [`docs/CONSTITUTION.md`](docs/CONSTITUTION.md) — non-negotiable principles
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — architectural decisions
- [`docs/FEATURES.md`](docs/FEATURES.md) — feature-level design choices
- [`docs/CONFIG.md`](docs/CONFIG.md) — every config file/env var/DB setting

Comments should explain **why**, not **what** — see the Comments section of
[`CLAUDE.md`](CLAUDE.md).

## Architecture

Thurbox follows **The Elm Architecture**
(`Event → Message → update → view → Frame`) with one-directional module
dependencies enforced by `tests/architecture_rules.rs`:

```text
session  ← pure data types, no crate-internal references
agent    → session
ui       → session + app (read-only model/view state)
app      → coordinator, imports all modules
```

A new module fails the architecture test until its dependencies are declared in
the allowlist. See [`docs/CONSTITUTION.md`](docs/CONSTITUTION.md) for the
non-negotiable rules.

## Pull requests

- Keep PRs focused — one logical change per PR.
- Include tests for new behavior and bug fixes.
- Make sure `just lint` and `just test` pass locally.
- Write a clear description of **what** changed and **why**.
- Update docs alongside code when a documented decision changes.

CI runs the same deterministic checks (clippy, nextest, cargo-deny, `cog`,
rumdl, shellcheck) that gate every merge — there are no LLM-gated checks.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE), the same license that covers the project.
