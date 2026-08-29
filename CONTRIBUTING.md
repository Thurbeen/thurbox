# Contributing to Thurbox

This guide covers setting up your environment, the conventions the project
follows, and how to get a change merged. Skimming [`README.md`](README.md),
[`CLAUDE.md`](CLAUDE.md) and the design docs under [`docs/`](docs/) first will
save you time.

Be respectful and constructive: assume good faith, keep discussions on the
technical merits, and help newcomers find their footing.

## Getting started

1. **Clone** the repository — you can push branches directly, no fork needed.
2. **Create a branch** off `main` (`git switch -c feat/my-change`).
3. **Set up the toolchain** (below).
4. **Make your change**, with tests.
5. **Run the checks** locally (`just lint && just test`).
6. **Push** and open a pull request with a clear description.

## Development environment

The reproducible dev environment is a **Nix flake** pinning the Rust toolchain,
`tmux`, `shellcheck`, Node, the cargo tooling, `just` and the demo stack.

```bash
nix develop          # enter the shell
direnv allow         # ...or, with direnv, auto-enter on cd (see .envrc)
```

No Nix? `scripts/install-dev-tools.sh` installs the dev tools (including
`prek`). You will also need `tmux >= 3.2`, `shellcheck`, `bats`, Node + npm (for
the website linters), `git`, and the three Lua gates `just lint` runs (`selene`,
`stylua`, `lua-language-server`).

MSRV is Rust 1.75, Edition 2021. The full walkthrough — including the runtime
sandbox for trying thurbox in isolation — is in
[`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

## Everyday tasks (`just`)

`just` is the task entrypoint — run it with no arguments for the full list.

| Task | What it does |
|------|--------------|
| `just build` | build the dev binaries (`thurbox` + `thurbox-cli`) |
| `just test` | `cargo nextest run --all` |
| `just lint` | fmt-check + clippy + cargo-deny + rumdl + shellcheck + selene, stylua and lua-language-server |
| `just fmt` | format Rust + website |
| `just arch` | architecture-rule + rustdoc checks |
| `just sandbox` | run thurbox in an isolated dev sandbox |

## Coding agents

Thurbox is agent-neutral, and so is the repo. [`CLAUDE.md`](CLAUDE.md) is the
canonical guidance doc — Claude Code reads it directly, and
[opencode](https://opencode.ai) loads it as project rules through its
Claude-Code compatibility (which only applies when no `AGENTS.md` exists, so
there is deliberately no `AGENTS.md` duplicating it).

One skill is checked in, `ui-review`, under `.claude/skills/`. opencode
auto-discovers that directory, so a single copy serves both agents — don't
mirror it under `.opencode/skills/`, which would double-register it. Slash
commands are the one kind opencode does **not** auto-discover, so a new one goes
in both `.claude/commands/` and `.opencode/commands/`, kept in sync by hand.
A minimal [`opencode.json`](opencode.json) declares the `$schema` for editor
validation.

### The no-mistakes gate

Reviewing and shipping a change is the job of
[no-mistakes](https://github.com/kunchenguid/no-mistakes), a local gate that
runs a change through one pipeline: intent, rebase, review, test, document,
lint, push, PR, then watching CI and rebasing the branch when the base moves.

Set it up once per checkout with `no-mistakes init`, then drive it from your
agent with `/no-mistakes` or by hand with `no-mistakes axi run --intent "..."`.
Its lint step runs `just lint` plus the rustdoc check, so it needs the same dev
toolchain the manual workflow does. It does not run the website linters — those
need `npm ci`, and CI's `website-lint` job covers them.

[`.no-mistakes.yaml`](.no-mistakes.yaml) configures it: the lint and format
commands, the paths excluded from review, the documentation ownership map, and
the per-path house rules the reviewer is given. It is external tooling, so it is
described here rather than in [`docs/CONFIG.md`](docs/CONFIG.md), which covers
thurbox's own configuration. The gate reads the fields that steer its behaviour
(`commands`, `document.instructions`, `review.path_instructions`) from **main**
rather than from your branch, so an edit to them only takes effect once merged.

The project's code-quality rubric lives in that file's
`review.path_instructions`, which is why there is no review command to run by
hand — extend those rules rather than reintroducing one.

One discipline the gate cannot do for you, because it validates committed
history rather than your working tree: **stage deliberately**. Commit the files
that belong to the change and nothing else — never `git add -A` on a dirty tree
— and keep credentials, `.env` files, keys, large binaries and scratch files
out. Leave unrelated uncommitted work where you found it; a change carrying
someone else's work in progress is one the reviewer cannot tell apart from
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
`tests/*.rs` one file per surface. Pane frames are pinned as literals in
`tests/frames.rs` — when a frame changes on purpose, the failing test prints
the new one to paste; there are no snapshot files and no tool to run. Crash
invariants are properties in `tests/render_props.rs`, and `tests/tui_e2e.rs`
drives the real binary on a real pty (`just smoke`). All of it runs in the one
`cargo nextest run --all`; see the Testing section of [`CLAUDE.md`](CLAUDE.md).

## Linting and formatting

CI runs a zero-warning policy — `clippy` and `rustdoc` warnings are errors.

```bash
cargo fmt --all                                            # format (100-char width)
cargo clippy --all-targets --all-features -- -D warnings   # lint
rumdl check .                                              # markdown lint
npm run lint:website                                       # website linters
```

`just lint` bundles the Rust + shell checks.

## Pre-commit hooks

[`prek`](https://github.com/j178/prek) runs the same checks CI does before each
commit. Install the hooks once after cloning:

```bash
prek install
```

- **commit-msg** — conventional-commit validation (`cog verify`)
- **pre-commit** — fmt, clippy, check, nextest, architecture rules, cargo-deny,
  rustdoc, bats, shellcheck, rumdl, prettier, htmlhint, stylelint, eslint
- **pre-push** — commit-history check (`cog check`)

## Commit conventions

All commits must follow
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

Commit type drives releases: `feat` → minor bump, `fix` / `perf` → patch bump;
`docs`, `chore`, `ci`, `style` and `test` produce no release.

## Documentation

If a change invalidates or extends a documented decision, update the relevant
doc in the **same PR**. Rationale lives in:

- [`docs/CONSTITUTION.md`](docs/CONSTITUTION.md) — non-negotiable principles
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — architectural decisions
- [`docs/FEATURES.md`](docs/FEATURES.md) — feature-level design choices
- [`docs/CONFIG.md`](docs/CONFIG.md) — thurbox's own config files, env vars and
  DB settings

Comments explain **why**, not **what** — see the Comments section of
[`CLAUDE.md`](CLAUDE.md).

## Architecture

Module dependencies are one-directional (`session ← agent ← kernel ← main`) and
enforced by `tests/architecture_rules.rs`: a new module fails that test until
its dependencies are declared in the allowlist. The graph is documented in the
Module Dependency Rules section of [`CLAUDE.md`](CLAUDE.md) and, with the full
per-module allowlist, in [`docs/CONSTITUTION.md`](docs/CONSTITUTION.md).

## Pull requests

- Keep PRs focused — one logical change per PR.
- Include tests for new behaviour and bug fixes.
- Make sure `just lint` and `just test` pass locally.
- Say **what** changed and **why**.
- Update docs alongside code when a documented decision changes.

CI runs the same deterministic checks (clippy, nextest, cargo-deny, `cog`,
rumdl, shellcheck) that gate every merge — there are no LLM-gated checks.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE), the same license that covers the project.
