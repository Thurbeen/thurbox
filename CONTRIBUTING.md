# Contributing to Thurbox

This guide covers setting up your environment, the conventions the project
follows, and how to get a change merged. Skimming [`README.md`](README.md),
[`AGENTS.md`](AGENTS.md) and the design docs under [`docs/`](docs/) first will
save you time.

Be respectful and constructive: assume good faith, keep discussions on the
technical merits, and help newcomers find their footing.

## Getting started

1. **Clone** the repository — push branches directly if you have write access,
   otherwise fork first and branch there.
2. **Create a branch** off `main` (`git switch -c feat/my-change`).
3. **Set up the toolchain** (below).
4. **Make your change**, with tests.
5. **Run the checks** locally (`just lint && just test`).
6. **Push** and open a pull request with a clear description.

## Development environment

The reproducible dev environment is a **Nix flake** (`nix develop`, or
`direnv allow` once); `scripts/install-dev-tools.sh` is the non-Nix fallback.
MSRV is Rust 1.95, Edition 2021. [`docs/thurbox/development.md`](docs/thurbox/development.md)
owns the setup — the toolchain, what to install without Nix, the `just` task
list, and the runtime sandbox for trying thurbox in isolation — so it is not
restated here.

## Everyday tasks (`just`)

`just` is the task entrypoint — run it with no arguments for the full list, or
read the table in [`docs/thurbox/development.md`](docs/thurbox/development.md#2-everyday-tasks--just).
The two to know before a pull request are `just lint` and `just test`.

## Coding agents

Thurbox is agent-neutral, and so is the repo. [`AGENTS.md`](AGENTS.md) is the
canonical guidance doc shared by coding agents. Keep repository instructions
there so every agent reads the same source.

The skills are checked in under `.agents/skills/`: per-subsystem
working references (`thurbox-testing`, `thurbox-kernel`, `thurbox-cli`,
… — `AGENTS.md` indexes them) plus `ui-review`. They carry the
detail that used to sit in `AGENTS.md`, so it stays an index and an agent loads
only the subject it is working on. `.agents/skills/` is the agent-neutral home —
the body of every skill lives there once, and each is exposed to a specific CLI
by a **relative symlink** from that CLI's own directory (`.claude/skills/<name>`
→ `../../.agents/skills/<name>` today). A new skill is authored in
`.agents/skills/` and symlinked, never the other way round. opencode
auto-discovers `.claude/skills/`, so those symlinks serve it too — don't mirror
anything under `.opencode/skills/`, which would double-register it.
[thurview](https://github.com/Thurbeen/thurview), the guided-review publisher,
is maintained in its own repository and is not vendored here. Slash
commands are the one kind opencode does **not** auto-discover, so a new one goes
in both `.claude/commands/` and `.opencode/commands/`, kept in sync by hand.
A minimal [`opencode.json`](opencode.json) declares the `$schema` for editor
validation.

### The publish gate

Reviewing and shipping a change is the job of the
[publish](https://github.com/LeTuR/publish) skill, which runs one pipeline:
rebase onto `main`, review the whole branch against this repository's own
rules, run the gate, commit what the review and the gate fixed, push, open the
pull request, then watch CI. It writes into the PR body an attestation naming
the commit every one of those steps ran against — a green pipeline on a commit
nobody will merge proves nothing, so a body whose attestation names anything
but the head is stale and says so.

Install it once per machine, then ask your agent to publish the branch:

```bash
npx skills@latest add https://github.com/LeTuR/publish --skill publish --agent claude-code --global --yes
```

[`.publish.yaml`](.publish.yaml) is everything the skill reads here: the base
branch, the two gate steps and the files the review must read. Its lint step
runs `just lint` plus the rustdoc check, so it needs the same dev toolchain the
manual workflow does. `just lint` does not carry rustdoc, but CI and the prek
hook do, so a broken intra-doc link would otherwise fail only after the push.
It does not run the website linters — those need
`npm ci`, and CI's `website-lint` job covers them. For the same reason its
`fix` command is not `just fmt`: it runs `cargo fmt` and `stylua` (which
`just lint` checks) and nothing that needs `node_modules`. A cancelled CI check
almost always means a newer push superseded the run: read the failing job
rather than re-running the pipeline. `.publish.yaml` is external tooling, so
it is described here rather than in [`docs/thurbox/config.md`](docs/thurbox/config.md), which
covers thurbox's own configuration.

The project's code-quality rubric is [`docs/thurbox/review.md`](docs/thurbox/review.md), which
`.publish.yaml` names in `review.rules`: the per-path house rules a reviewer
reading only the diff could not know, the trees that carry no reviewable
intent, and the documentation ownership map. There is no review command to run
by hand — extend those rules rather than reintroducing one. They live on your
branch and take effect there, so a change that weakens them is a change the
pull request shows.

Performance is reviewed there rather than measured there, deliberately. The
`src/**` block carries the render loop's change-signal rules — including that a
compare-before-store is not enough for anything recomputed per frame from a
source that moves continuously, which is the failure ADR-P20 records — and asks
a change claiming a performance effect to carry a paired before/after from
`just bench` or `just perf`. What the gate does *not* do is time anything: that
would be a flaky assertion about the machine it happened to run on, which
[`docs/thurbox/performance.md`](docs/thurbox/performance.md) rules out in ADR-P5. The
deterministic half is ordinary test coverage — counters and change-signals in
`tests/kernel_perf.rs` and `tests/kernel_frame_cost.rs` — and the lint step
compiles `benches/` through `cargo clippy --all-targets`, so the instrument
cannot rot while nobody is running it.

The load harness stays out of the gate by refusing to run there
(`THURBOX_GATE`). It lives under `scripts/dev/`, drives the real binary and
prints a result, so it reads as a test to anything deciding what "run the tests"
means — and a step that waits through a release build and timed runs fails on
the agent timeout, which is what happened once. That is also why the `test` step
names the suite explicitly rather than leaving the choice to whatever is driving
the gate.

The fixes the gate commits are ordinary conventional commits, so the
`commit-msg` hook — the one place a branch commit is still refused — accepts
them. `chore` is the type to reach for: it is scopeless (`cog.toml` enumerates
the valid scopes and a gate fix spans them) and omitted from the changelog, and
under squash merge no branch commit reaches the release decision anyway; the
pull request title does.

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
`cargo nextest run --all`; see the `thurbox-testing` skill under
`.agents/skills/`.

## Linting and formatting

CI runs a zero-warning policy — `clippy` and `rustdoc` warnings are errors.

```bash
cargo fmt --all                                            # format (100-char width)
cargo clippy --all-targets --all-features -- -D warnings   # lint
rumdl check .                                              # markdown lint
npm run lint:website                                       # website linters
```

`just lint` bundles all of these except the website linters, plus cargo-deny
and the three Lua gates (`selene`, `stylua`, `lua-language-server`).

## Pre-commit hooks

[`prek`](https://github.com/j178/prek) runs the checks CI does, in two speeds:
the fast ones before each commit, the ones needing a full build before each
push. Install the hooks once after cloning (a clone that installed them before
the pre-push stage existed needs this run again):

```bash
prek install
```

- **commit-msg** — conventional-commit validation (`cog verify`)
- **pre-commit** — fmt, clippy, large files, bats, shellcheck, rumdl (and a check that the
  local rumdl is the version CI pins), selene, stylua, prettier, htmlhint,
  stylelint, eslint
- **pre-push** — nextest (the architecture rules included), rustdoc,
  cargo-deny, and Windows clippy cross-checked from Linux
  (`just clippy-windows`)

Commit messages are not checked at push: squash merge discards them — see
below.

## Commit conventions

All commits must follow
[Conventional Commits](https://www.conventionalcommits.org/), enforced locally
by `cocogitto` in the `commit-msg` hook. That hook keeps the branch's history
legible; it is not what gates the merge — the pull request title is, because
that is the only message that survives the squash.

- **Types:** `feat`, `fix`, `perf`, `refactor`, `docs`, `style`, `test`,
  `chore`, `ci`, `build`, `revert`
- **Scopes:** `api`, `cli`, `ui`, `git`, `core`, `docs`, `deps`, `config`,
  `mcp`, `fleet`, `runtime`

```bash
cog commit feat "add remote host picker"
cog commit fix "avoid panic on empty worktree" git
```

Commit type drives releases: `feat` → minor bump, `fix` / `perf` → patch bump;
`docs`, `chore`, `ci`, `style` and `test` produce no release. On `main` the type
that counts is the **pull request title's** — see below.

### The pull request title is the commit

Pull requests land by **squash merge**, so the commit that reaches `main` is not
any commit from your branch: GitHub builds it from the pull request title plus
its own `(#N)` suffix.

There is no merge queue: at this repository's merge cadence it batched nothing
and merged one pull request at a time behind a second full CI run. Reintroducing
one would need a `merge_group:` trigger back in `ci.yml` — without it the
required `All Checks` gate never reports on a queued pull request and the queue
stalls until its own check timeout.

```text
fix(core): keep the caret where the frame put it
  ↓ squash merge
fix(core): keep the caret where the frame put it (#1044)
```

That single string is what `cog bump --auto` reads for the release decision and
what the changelog quotes. Nothing else validates it: CI no longer walks the
branch's commits, because squash discards exactly those. So the type and scope
allowlists apply to the **title**, and it is the title's type that decides the
release — a branch whose every commit is `fix`
still ships nothing if its pull request is titled `docs:`. Keep a pull request
to one purpose and title it after its most significant change.

Declare a breaking change in the title (`feat(core)!: …`) or as a
`BREAKING CHANGE:` footer in the pull request **body**. The squash commit's body
comes from the body box, so a footer left behind in a branch commit is
discarded.

[`scripts/ci/check-pr-title.sh`](scripts/ci/check-pr-title.sh) enforces this as
the required `PR Title` check. It validates the title in the exact form that
lands, suffix included, and rejects one that already ends in a `(#N)` of its own
— squash would append a second. When it rejects a title it prints the types and
scopes `cog.toml` declares, so the message is enough to correct it. It runs from
its own workflow rather than from CI because `ci.yml` does not listen for
`edited`, so a title changed after checks went green would otherwise never be
revalidated; its bats suite runs in CI's `PR Title Checker` job so that workflow
stays lean.

## Documentation

If a change invalidates or extends a documented decision, update the relevant
doc in the **same PR**. Rationale lives in:

- [`docs/constitution.md`](docs/constitution.md) — non-negotiable principles
- [`docs/thurbox/architecture.md`](docs/thurbox/architecture.md) — architectural decisions
- [`docs/thurbox/features.md`](docs/thurbox/features.md) — feature-level design choices
- [`docs/thurbox/config.md`](docs/thurbox/config.md) — thurbox's own config files, env vars and
  DB settings
- [`docs/thurbox/review.md`](docs/thurbox/review.md) — the house rules a change is reviewed
  against, and which document owns which class of fact

Code carries no comments: names, types, small functions and tests say what it
does and why. The [`no-comments`](.agents/skills/no-comments/SKILL.md) skill
lists the few exceptions and the `MUST KILL` marker.

## Architecture

Module dependencies are one-directional and enforced by `arch-rules` over the
module rules in `crates/architecture.toml`: a new module fails its check until
its dependencies are declared there. That file is the one copy of the graph;
[`docs/constitution.md`](docs/constitution.md) T1 states the rule.

## Pull requests

- Keep PRs focused — one logical change per PR.
- Include tests for new behaviour and bug fixes.
- Make sure `just lint` and `just test` pass locally.
- Say **what** changed and **why**.
- Update docs alongside code when a documented decision changes.

CI runs the same deterministic checks (clippy, nextest, cargo-deny, `cog`,
rumdl, shellcheck, the large-file cap) that gate every merge — there are no
LLM-gated checks.

### Large files

Git keeps every version of every file, and every clone downloads all of them.
Re-recorded demo GIFs and MP4s grew this repository to 478 MiB packed while its
tree is 39 MiB. Deleting a file later does not give that space back.

- prek's builtin `check-added-large-files` hook fails a file over 1 MiB. It
  runs on staged files at commit, and CI's `Large Files` job runs it on the
  whole tree.
- Its `exclude` in `.pre-commit-config.yaml` lists the few files that were over
  the cap when it arrived. Shrink a file and remove it from that list; do not
  add to it.
- Demo recordings are not committed. `scripts/demo/publish-media.sh` uploads
  them to the `media` release, where the README, the docs and the website read
  them.
- Put generated output in `.gitignore` and build it where it is used, or attach
  it to a release.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE), the same license that covers the project.

## Mod gallery

The [Mods page](https://thurbox.thurbeen.eu/mods.html) lists the public GitHub
repositories that carry the [`thurbox-mods`](https://github.com/topics/thurbox-mods)
topic. The site build searches GitHub for the topic, so no pull request is needed.
To list a mod:

1. Publish the mod in a public GitHub repository.
2. Add the `thurbox-mods` topic to the repository.
3. Link setup instructions in your README: required Thurbox versions, capability
   grants, dependencies, platform limits and conflicts with other panes.
4. Optional: add a preview and a `[gallery]` table, as described below.

Forks and archived repositories are left out. Repositories owned by `Thurbeen`
show an **Official** badge; every other one shows **Community**. Community
listings are not reviewed.

### What the gallery reads

| Field on the card | Source |
|---|---|
| Name, owner, description, stars, last update, topics | The GitHub repository |
| Version, required Thurbox version | `version` and `requires_thurbox` in `mod.toml` |
| Description, when the repository has none | `description` in `mod.toml` |
| Preview | `[gallery] media`, else `media/demo.gif` when the repository has one |
| Install command | `thurbox-cli mod install git+<repository URL>`; `[gallery] install` replaces it for `Thurbeen` repositories only |
| Platforms, setup notes | `[gallery] platforms` and `notes` |

The `[gallery]` table goes in the `mod.toml` at the repository root, beside the
manifest fields `thurbox-cli mod install` reads. Every field is optional:

```toml
[gallery]
media = "media/demo.gif"
media_alt = "What the screenshot shows"
install = "thurbox-cli mod install git+https://github.com/you/thurbox-widget"
platforms = ["Linux", "macOS"]
notes = "Required capability grants, layout changes and conflicts."
```

`media` is a path in the repository or an HTTPS URL. A path cannot leave the
repository. The site displays the install command; it never runs it. Because
community listings are not reviewed, the site reads `install` only from
`Thurbeen` repositories. A community mod that needs another command asks for it
in the curated overlay, which maintainers review.

### Curated overlay

[`website/_data/mods.json`](website/_data/mods.json) holds what GitHub cannot
supply. Its schema is [`website/mods.schema.json`](website/mods.schema.json).
Each entry names a `repo` and can set:

- `install`, `platforms`, `notes` and `media` (`url` and `alt`). These replace
  the repository's `[gallery]` values. A reviewed `install` here is the only way
  a community mod shows a command other than the default.
- `hidden: true`, which removes the repository from the gallery.

A repository in the overlay is listed even without the topic. That keeps the
mods published before the topic existed on the page.

### Build, refresh and fallback

- The site build reads the GitHub API with the workflow's token (`GITHUB_TOKEN`
  or `GH_TOKEN`). The Deploy Website workflow runs every day, so new and
  updated mods appear within a day.
- If the search fails, is rate-limited or returns incomplete results, the build
  uses [`website/mods-snapshot.json`](website/mods-snapshot.json) and logs a
  warning. A failed fetch never breaks the build or empties the page.
- `npm run mods:snapshot` refreshes the snapshot. `THURBOX_GALLERY_OFFLINE=1`
  (`npm run build:website:offline`) builds from the snapshot without network
  calls; `npm run lint:website` builds this way.
- `npm run check:mods:links` checks the overlay's URLs and runs a live discovery.
- `npm run test:mods` tests discovery, the fallback and the page against a
  mocked GitHub API.

## Fleet plugin gallery

The [Fleet plugins page](https://thurbox.thurbeen.eu/fleet-plugins.html) lists
the public GitHub repositories that carry the
[`thurbox-fleet-plugins`](https://github.com/topics/thurbox-fleet-plugins) topic.
A Fleet plugin adds resource kinds to Fleet; a mod changes the thurbox
interface, and has its own [gallery](#mod-gallery). Both pages run on the same
code (`scripts/ci/gallery.mjs`), so discovery, badges, the overlay, the build
and the fallback work as the mod gallery describes. Only these differ:

| | Mods | Fleet plugins |
|---|---|---|
| Topic | `thurbox-mods` | `thurbox-fleet-plugins` |
| Manifest at the repository root | `mod.toml` | `fleet-plugin.toml`, read only by the site |
| Fields read besides `[gallery]` | `version`, `requires_thurbox`, `description` | `version`, `requires_fleet`, `kinds`, `description` |
| Default install command | `thurbox-cli mod install git+<repository URL>` | `git clone <repository URL> /opt/fleet-plugins/<name>` |
| Curated overlay | [`website/_data/mods.json`](website/_data/mods.json) | [`website/_data/fleet-plugins.json`](website/_data/fleet-plugins.json) |
| Snapshot | [`website/mods-snapshot.json`](website/mods-snapshot.json) | [`website/fleet-plugins-snapshot.json`](website/fleet-plugins-snapshot.json) |
| Scripts | `check:mods`, `test:mods`, `check:mods:links`, `mods:snapshot` | `check:fleet-plugins`, `test:fleet-plugins`, `check:fleet-plugins:links`, `fleet-plugins:snapshot` |

`kinds` lists the CamelCase kind names the plugin's `manifest` call answers;
the page drops any other value. [Write a plugin](docs/fleet/plugins.md#step-6-publish-it)
documents `fleet-plugin.toml` for plugin authors. Both overlays share one
schema, [`website/mods.schema.json`](website/mods.schema.json).
