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
| `just lint` | fmt-check + clippy + cargo-deny + rumdl + shellcheck + selene, stylua and lua-language-server |
| `just fmt` | format Rust + website |
| `just arch` | architecture-rule + rustdoc checks |
| `just sandbox` | run thurbox in an isolated dev sandbox |

## Coding agents

Thurbox is agent-neutral, so the repo works with any coding-agent CLI.
[`CLAUDE.md`](CLAUDE.md) is the canonical guidance doc — Claude Code reads it
directly, and [opencode](https://opencode.ai) loads it automatically as project
rules via its Claude-Code compatibility (it's picked up when no `AGENTS.md`
exists, so there's deliberately no `AGENTS.md` duplicating it).

One skill is checked in, `ui-review`, under `.claude/skills/`. opencode
auto-discovers that directory, so a single copy serves both agents — don't
mirror a skill under `.opencode/skills/`, which would double-register it. A
minimal [`opencode.json`](opencode.json) declares the `$schema` for editor
validation.

There are no checked-in slash commands: `/publish`, `/refactor`, `/ship` and
`/sync` were replaced by the no-mistakes gate below, and `.opencode/` went with
them. Commands are the one kind opencode does **not** auto-discover from
`.claude/`, so if you add one, put it in both `.claude/commands/` and
`.opencode/commands/` and keep the two in sync by hand.

### The no-mistakes gate

Reviewing and shipping a change is the job of
[no-mistakes](https://github.com/kunchenguid/no-mistakes), a local gate that
runs the change through one pipeline — intent, rebase, review, test, document,
lint, push, PR, then watching CI and rebasing the branch itself when the base
moves. It replaces the checked-in `/publish` skill and the `/refactor`, `/ship`
and `/sync` commands, which did the same work by hand and with none of that
pipeline's gating.

Set it up once per checkout with `no-mistakes init`, then drive it from your
agent with `/no-mistakes` or by hand with `no-mistakes axi run --intent "..."`.
The lint step runs `just lint` plus the rustdoc check, so the gate needs the
same dev toolchain the manual workflow does. It does not run the website
linters — those need `npm ci`, and CI's `website-lint` job covers them.

[`.no-mistakes.yaml`](.no-mistakes.yaml) at the repo root configures it — the
lint and format commands, the paths excluded from review, this repo's
documentation ownership map, and the per-path house rules the reviewer is given.
That file is its own owner: it is external tooling, so it is described here
rather than in [`docs/CONFIG.md`](docs/CONFIG.md), which covers thurbox's own
configuration. Note that the gate reads the fields that steer its behaviour
(`commands`, `document.instructions`, `review.path_instructions`) from **main**
rather than from your branch, so an edit to them only takes effect once merged.

The code-quality rubric those workflows carried did not go away with them:
it is the per-path guidance in `review.path_instructions`, so the gate's
reviewer applies it to every change instead of only to the changes someone
remembered to run a command on. Extend that file rather than reintroducing a
review command.

One discipline the gate cannot do for you, because it validates committed
history rather than your working tree: **stage deliberately**. Commit the files
that belong to the change and nothing else — never `git add -A` on a dirty
tree — and keep credentials, `.env` files, keys, large binaries and scratch
files out of the commit. Preserve unrelated uncommitted work you found in the
tree instead of sweeping it into the branch. A change that carries someone
else's work in progress is one the reviewer has no way to tell apart from
yours.

## Testing

Thurbox follows **test-driven development** — write a failing test first, make
it pass, then refactor. Bug fixes start with a test that reproduces the bug.

```bash
cargo nextest run --all              # run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # run a single test by name
```

The interface is Lua on a Rust kernel, so most coverage drives the **real kernel
over the real `ui/`**: `tests/kernel_mvp.rs` for the kernel's contract and
`tests/v2_*.rs` one file per surface. Pane frames are pinned as literals in
`tests/v2_frames.rs` — when a frame changes on purpose, the failing test prints
the new one to paste; there are no snapshot files and no tool to run. Crash
invariants are properties in `tests/v2_render_props.rs`, and `tests/tui_e2e.rs`
drives the real binary on a real pty (`just smoke`). All of it runs in the one
`cargo nextest run --all`. See the Testing section of [`CLAUDE.md`](CLAUDE.md)
for the full picture.

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
- [`docs/CONFIG.md`](docs/CONFIG.md) — thurbox's own config files, env vars and
  DB settings

Comments should explain **why**, not **what** — see the Comments section of
[`CLAUDE.md`](CLAUDE.md).

## Architecture

Module dependencies are one-directional (`session ← agent ← kernel ← main`) and
enforced by `tests/architecture_rules.rs`. A new module fails the architecture
test until its dependencies are declared in the allowlist.

The graph itself is documented once — in the Module Dependency Rules section of
[`CLAUDE.md`](CLAUDE.md) and, with the full per-module allowlist and the
fully-qualified-path crossings, in [`docs/CONSTITUTION.md`](docs/CONSTITUTION.md).
Rationale is in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

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
