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
sourced by the sandbox entrypoint plus `scripts/demo/record.sh` and
`scripts/dev/smoke/tui-smoke.sh` (which use the full flavor). Single source of
truth for the `thurbox-dev` sandbox pattern.

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
bats scripts/install.bats            # Test install script (requires bats-core)
```

### Kernel and interface tests

The interface is Lua on a Rust kernel, so most coverage drives the **real
kernel over the real `ui/`** rather than a harness that imitates either:

- **`tests/kernel_mvp.rs`** — the kernel's contract: the four node kinds and their
  count, the plugin environment enumerated global-by-global (no blanket exemption
  for a leading underscore — that is how a capability once hid under `__run_impl`),
  the instruction/memory bounds, snapshot reads, and painting a plugin to a
  `TestBackend`.
- **`tests/v2_*.rs`** — one file per surface or contract: `v2_session_list`,
  `v2_search`, `v2_new_session`, `v2_terminal_pane`, `v2_session_lifetime`,
  `v2_keymap`, `v2_focus`, `v2_modals`, `v2_chrome`, `v2_mouse`, `v2_hover`,
  `v2_decoration`, `v2_plugin_{authoring,commands,lifecycle,settings,switching}`,
  `v2_repo_memory`, `v2_remote_status`, `v2_session_status`, `v2_core_settings`,
  `v2_attach_by_name`.
  Several build an interface in a tempdir from the embedded copy, so delivery and
  loading are exercised together.
- **`tests/kernel_limits.rs`** — instruction and memory ceilings, in their own file
  because they mutate process-wide limits.
- **Lua statics** — `selene ui` (undefined names + the sandbox, via `thurbox.yml`),
  `lua-language-server --check` (types + withheld libraries), `stylua` (format).
  The three cover different halves; see **Linting & Formatting**.
- **Black-box smoke test** (`scripts/dev/smoke/tui-smoke.sh`) — launches the real
  binary in a throwaway tmux pane with isolated `HOME`/XDG/`TMUX_TMPDIR` and
  asserts on captured frames. Gated behind the `tui-smoke` CI job (needs tmux).

Tests that shell out to `git` **must scrub the `GIT_*` location variables**
(`git::GIT_LOCATION_ENV`): git exports them to hook processes, so the suite running
under this project's own pre-commit `cargo nextest` inherits a `GIT_DIR` pointing at
the real repository. `tests/v2_repo_memory.rs` and `tests/create_e2e.rs` show the
shape.

> v1's in-process acceptance harness, its `insta` snapshots, its invariant monkey
> test and `tests/v1_recordings.rs` were deleted with `src/app`. They are in the
> history if a behaviour needs archaeology.

### Session-backend e2e harnesses

One family under `scripts/dev/e2e/` (`linux-container.sh` = ephemeral Podman,
`windows-vm.sh` = ephemeral dockur Windows VM, `real-host.sh` = a machine you own)
sharing `e2e/lib/e2e-common.sh` — colour logging, the PASS/FAIL contract
(`E2E_JSON=1` for a machine-readable line), an in-shell `json_field` extractor (no
`python3`), the `[[hosts]]` emitter, and the `session create → get → assert` core.
`scripts/dev/README.md` is the newcomer index and carries the old→new path map.

## Performance (render loop)

The loop is **demand-driven**: it paints when something changed or when the 250 ms
forced-redraw floor (`FORCE_REDRAW_INTERVAL`) elapses, never on every iteration.
There are **two floors between paints**, because typing has to feel instant and
watching a log scroll does not: `MIN_FRAME_INTERVAL` (16 ms) when a person did
something — a key, a resize, a worker result they asked for, tracked by
`input_dirty`, which is set with `dirty` through the one `App::note_input` and
never alone — and `OUTPUT_FRAME_INTERVAL` (33 ms) when the only thing owing a
frame is agent output. Applying the tight floor to both made a chatty agent drive
60 paints a second to show 30 lines; the split is worth 30% of the loaded cost
(ADR-P17). What marks the screen dirty:
any input, a resize, a reload, a worker result, and **new agent output** —
`Terminals::output_generation` is summed each iteration, which is what stops a
printing agent being drawn at 4 fps. A **reflow** — the arrangement placing a
slot at a new rect, so a column opened or closed — additionally forces one *full*
repaint: the cell diff is only correct while ratatui and the terminal agree on a
glyph's width, and where they cannot (a flag, an emoji presentation sequence) the
pane that closed leaves characters behind.
`kernel::paint::normalize_ambiguous_width` strips the one such disagreement that
is strippable — `U+FE0F` — from every painted cell, panes and vt100 surfaces
alike, and `kernel::paint::force_full_repaint` covers the rest by marking every
cell of the reflowed frame as one the diff must print. It is deliberately **not**
`Terminal::clear`: erasing flushes a blank screen and leaves the repaint to the
next flush, so every pane toggle blinked the whole interface.

A frame is more expensive than v1's, structurally: every pane is a Lua call
returning a table that is converted to nodes and painted. The **conversion** is
the surprise in that sentence and the biggest single cost in a frame — bigger
than running the plugins' Lua. `kernel::convert` therefore reads each node's
fields in **one `pairs` pass** rather than ~25 keyed lookups, and builds its
error paths as a borrowed chain (`Crumb`) rather than a `String` per node; both
are measured in `docs/PERFORMANCE.md`. So the loop settles
aggressively. `draw` compares each plugin's returned tree against the last one and
only marks the frame changed when it differs; a float does the same against its own
last tree and rect, and a **chrome band** compares the *cells* it just painted
against the ones it painted last frame — it has no tree to diff, and marking it
changed for having been *drawn* held `dirty` set after every frame, which stopped
the loop settling at all: an idle interface with no sessions repainted at the frame
cap forever (~32% of a core, against ~6% once it settles). Neither an open float
nor a live text selection marks the frame changed by itself — both used to,
which pinned the loop at the frame cap for as long as the creation wizard was
open. The perf HUD is the deliberate exception: its
counters move every iteration, so it says so.

Everything that touches the world runs on a worker and publishes back (rule 5):
`kernel::terminal` (attach — the sharpest teeth, since a down host runs out its ssh
timeout and adopting a pane needs the runtime *entered* on the worker),
`kernel::command`, `kernel::diff`, `kernel::metrics` (three cadences, one published
result — the clearest one to copy), `kernel::repos` (the only *parameterised* reads,
asked for by leaving a key in `store`), `kernel::runs`, `kernel::updates`.

Cached answers carry an **age**, not just a value. The mistake this repeatedly
invited was storing "we have an answer" where "the answer is current" was needed:
git stats froze at their first reading, a `run` refresh started a process per frame,
a failed branch fetch stuck for the process lifetime, and a backend surveyed once
was treated as surveyed since. Each is now a TTL, an in-flight marker, or a
generation counter — if you add a cache here, give it one.

`republish` — the one call that rebuilds every `thurbox.*` table — runs once per
painted frame and **once per input batch**, not once per event: a held-down key
otherwise paid for it per repeat. Within it each group is **gated on a
change-signal** (`SnapshotStore::version`, `Themes`/`Registry::version`,
`Terminals::meta_version`/`failed_version`, and the loop's `data_epoch`), so a
group whose inputs did not move is not rebuilt; and a pane that declares
`pure = true` has the tree it last returned reused instead of being run at all.
Both are ADR-P16, and both rest on one rule: a signal is bumped **inside** the
mutation and only when the value actually changed — writing an unchanged value
counts as no change, which is the difference between the gate saving 27% and
saving nothing. The **animation clock** obeys it too: it lives in the epoch and
the loop advances it only while something is actually animating, because a
free-running one invalidated every pure pane on every idle frame. And the loop
itself slows its input poll to `IDLE_TICK` once nothing has happened for
`QUIESCENT_AFTER` — free, because `event::poll` returns the instant an event
arrives, so only things that never wake the thread are delayed.

The three reads in `republish` that touch a screen or the
disk carry the age above (ADR-P14): link extraction is keyed on that session's
`output_stamp` **and limited to surfaces actually on screen** (a link nothing
painted can be neither clicked nor handed to the outer terminal, and the scan
walks a whole vt100 grid — doing it for every live pane cost ~1.2ms a frame with
three of them), the search content scan on `output_generation`, and the interface
inventory's per-file digests on a `trust_stale` flag every path that changes the
directory or a grant already sets.

**A vt100 grid is never given fewer than two rows or two columns**
(`agent::backend::vt_floor`). A cramped layout really does compute a one-cell pane,
and vt100 underflows on the next byte written into one — in `row_inc_scroll` when a
line wraps, in `col_wrap` (`cols - width`) when a double-width character arrives.
The panic lands on the session's *reader* thread, so the process lives while that
session's terminal is blank for the rest of the run: the unwind poisons the parser
mutex, and every reader of it (paint, links, selection, copy) reads a poisoned lock
as "no live terminal". A panic is also written to `thurbox.log`, because a worker's
stderr is scrolled away long before anyone looks.

**Observability**: `F12` toggles the perf HUD (`[features] perf_hud`); launching with
`THURBOX_PERF_LOG=1` writes `startup`, `perf_window` and `slow op` lines to
`thurbox.log`; while either is active a JSON snapshot is published for
`thurbox-cli perf`. Three histograms, kept separate so they **decompose** rather
than nest: `frame` is the paint, `republish` is the per-frame table rebuild
above, and `tick` is the rest of one iteration. `kernel::perf::snapshot_json`
owns the published shape and `cli::perf` only renders it. Full rationale:
`docs/PERFORMANCE.md`.

## Installation Script

**Linux / macOS** — `scripts/install.sh`:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows** — `scripts/install.ps1` (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

Both installers share the same shape: ASCII banner, platform detection, version
resolution (GitHub API → releases-page scrape fallback), SHA256 checksum
verification, extract, post-install hints. From the same release, `install.sh`
pulls the `.tar.gz` for `x86_64-unknown-linux-musl` / `aarch64-apple-darwin`
(Linux x86_64 + Apple-silicon macOS — the only platforms it installs onto; it
errors cleanly on any other), while `install.ps1` pulls
**`thurbox-<ver>-x86_64-pc-windows-msvc.zip`** (the Windows artifact built by
`cd.yml`) and extracts it with the built-in `Expand-Archive` (no tar needed).
ARM64 Windows installs the x86_64 build (runs under x64 emulation).

**`install.sh` (POSIX `sh`) specifics:**

- Colorized output (auto-disabled when stderr is not a TTY, `NO_COLOR` is set,
  or `TERM=dumb`); platforms Linux/macOS × x86_64/aarch64
- No external deps beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Env vars: `VERSION=v1.0.0`, `INSTALL_DIR=/path` (default `~/.local/bin`)
- Non-interactive (safe pipe-to-shell), cleanup via `trap`
- Tested by `scripts/install.bats` (bats-core, ~28 tests; CI `install-script` job)

**`install.ps1` (PowerShell 5.1+) specifics:**

- Parameters `-Version` / `-InstallDir` / `-Repo`, or the matching
  `THURBOX_VERSION` / `THURBOX_INSTALL_DIR` / `THURBOX_REPO` env vars (env vars
  are the reliable path for the `irm | iex` form, which can't pass parameters);
  default install dir `%LOCALAPPDATA%\Programs\thurbox`
- Adds the install dir to the **user** `PATH` (`[Environment]::SetEnvironmentVariable(... 'User')`)
  when missing; reflects it into the current session
- ASCII-only source (no BOM needed; survives `irm | iex` decoding on Windows
  PowerShell 5.1); `Write-Host` for UI is intentional (`Write-Output` would leak
  into the `iex` pipeline)
- Pure helpers (`Get-Target`, `Get-ExpectedChecksum`) are guarded by
  `$env:THURBOX_PS_TEST` so the file can be dot-sourced for testing without
  running the installer
- Tested by `scripts/install.Tests.ps1` (Pester 5; CI `install-script-ps` job,
  run with `pwsh` on ubuntu since the helpers are platform-independent) —
  the PowerShell mirror of `install.bats`

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
removed, which is the same shape the VM enforces (design.md D9) and means a plugin
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

## Release Process

Releases are **fully automated** via GitHub Actions. No version commits
are created - version is determined by git tags only.

### How It Works

Every push to `main` automatically triggers the release workflow:

1. **Commit Analysis**: Analyzes all commits since last tag using cocogitto
2. **Release Decision** — a release needs **both** gates to pass:
   - **Commit type** (`check-release`'s `parse` step): commits must include
     `feat`, `fix`, or `perf`. Only docs/chore/ci commits → no release.
   - **Artifact relevance** (`check-release`'s `shipped` step): the diff since
     the last tag must touch something a user installs (`src/`, `tests/`,
     `build.rs`, `Cargo.toml`/`Cargo.lock`, `rust-toolchain.toml`, `Cross.toml`,
     `extensions/`, `ui-plugins/`, `packaging/`, `scripts/install.{sh,ps1}`,
     `cd.yml`). `extensions/` and `ui-plugins/` are there although no binary
     carries them: a bare-name install resolves against the release *tag*, so a
     change never tagged is one nobody can install.
     Commit type alone over-releases: Renovate labels a GitHub-Actions pin bump
     `fix(deps)` and the website is versioned `feat(ui)`/`fix(ui)`, so a
     CSS-only or lint-action-only change used to cut a real release (v1.2.13
     through v1.3.0 were four website-only releases, one a *minor*) — burning a
     4-platform build and pushing to the moderated Chocolatey/winget channels
     for a no-op binary. Such commits stay in history and ride along in the
     next real release's changelog.
   - The gate is evaluated over the **whole span since the last tag**, not the
     single push, so a website-only push landing on an unreleased `src/` commit
     still cuts the release it owes. A forced `workflow_dispatch` version
     **skips** the relevance gate — an explicit human cut is always honoured.
3. **Automated Release** (if needed):
   - Determines semantic version (feat→minor, fix/perf→patch, breaking→major)
   - Creates lightweight git tag: `v{version}` (e.g., v1.0.0)
   - Pushes tag to origin
   - Builds binaries for 4 platforms (3 Unix `.tar.gz` + 1 Windows `.zip`;
     version passed via environment variable)
   - Generates changelog from commits
   - Publishes GitHub Release with binaries and release notes

### Version Management

- **Cargo.toml version**: Always `0.0.0-dev` (static development marker)
- **Real version**: Determined by release workflow (v1.0.0, v1.1.0, etc.)
- **Build-time injection**: `build.rs` uses `THURBOX_RELEASE_VERSION` environment
  variable (set by workflow) to inject version into binary
- **Development builds**: Show `0.0.0-dev` (when `THURBOX_RELEASE_VERSION` not set)
- **Release builds**: Show actual version (e.g., `1.0.0`) via env variable from workflow
- **Explicit cuts**: `cog bump --auto` computes the next version from commits and
  works for every ordinary release (at 1.x+, a breaking change correctly bumps
  the major). To cut a *specific* version that `--auto` can't reach — the only
  way across a major boundary from a `0.x` line, or any one-off — dispatch the
  Release workflow (`cd.yml`) with the `version` input (e.g. `1.0.0`); it runs
  `cog bump --version <v>` instead of `--auto`.

### Release Artifacts

Each release includes:

- Binaries for 4 platforms:
  - `thurbox-v{ver}-x86_64-unknown-linux-gnu.tar.gz`
  - `thurbox-v{ver}-x86_64-unknown-linux-musl.tar.gz`
  - `thurbox-v{ver}-aarch64-apple-darwin.tar.gz`
  - `thurbox-v{ver}-x86_64-pc-windows-msvc.zip` (the Windows artifact
    extracted by `install.ps1` / packaged by Chocolatey + winget)
- `thurbox-v{ver}-checksums.txt` (SHA256 sums for verification)
- Changelog with categorized commits

### Distribution Packages

After the GitHub Release is published, `cd.yml` also updates the downstream
package channels (each gated on its secret, skipped on forks):

- **Homebrew** (`publish-homebrew`): bumps `version`/`sha256` in
  `packaging/homebrew/Formula/thurbox.rb` (via `packaging/homebrew/bump-formula.py`,
  reading the release `checksums.txt`) and pushes it to the
  `Thurbeen/homebrew-thurbox` tap over SSH. Needs the `HOMEBREW_TAP_DEPLOY_KEY`
  secret (a write deploy key on the tap repo; the org blocks cross-repo PATs).
  Install: `brew install thurbeen/thurbox/thurbox`. Supports macOS arm64
  (`aarch64-apple-darwin`) + Linux x86_64 (`x86_64-unknown-linux-musl`).
- **AUR** (`publish-aur`): bumps + pushes `thurbox`/`thurbox-bin` PKGBUILDs.
  Needs `AUR_SSH_PRIVATE_KEY`.
- **Chocolatey** (`publish-chocolatey`): bumps `<version>` in
  `packaging/chocolatey/thurbox.nuspec` and `$url64`/`$checksum64` in
  `tools/chocolateyinstall.ps1` (via `packaging/chocolatey/bump-nuspec.py`,
  reading the release `checksums.txt`), then `choco pack` + `choco push` to the
  community repo. Runs on `windows-latest`; needs the `CHOCOLATEY_API_KEY`
  secret. New versions go through community-repo moderation.
  Install: `choco install thurbox`. Windows x86_64 only.
  **Throttled to one push per `THROTTLE_DAYS` (30d) window** because the
  community repo moderates + rate-limits every push and can't keep up with
  thurbox's per-`feat`/`fix`/`perf` cadence (versions pile up → `choco push`
  returns **403**). The job queries the community OData feed
  (`community.chocolatey.org/api/v2/Packages()`) for the last-published
  version's age; younger than the window ⇒ **skip the push, exit green** with a
  `::warning::` (patch releases coalesce into the next monthly Chocolatey
  version — the binary still ships immediately via GitHub Releases +
  Homebrew/AUR/winget). A residual `403`/`409` at push time is likewise caught
  and exits green; only a genuine failure (bad package, auth) fails red — so a
  backed-up channel never turns the whole release red.
- **winget** (`publish-winget`): bumps `PackageVersion`/`InstallerUrl`/
  `InstallerSha256`/`ReleaseNotesUrl` in the three manifests under
  `packaging/winget/manifests/` (via `packaging/winget/bump-manifests.py`,
  reading the release `checksums.txt`), then `wingetcreate submit`s the set as a
  PR to `microsoft/winget-pkgs`. Runs on `windows-latest`; needs the
  `WINGET_TOKEN` secret (a `public_repo` PAT owning a fork of
  `microsoft/winget-pkgs`). New versions go through winget-pkgs PR
  validation + review.
  **Throttled to one submission per `THROTTLE_DAYS` (30d) window**, mirroring
  Chocolatey, because winget-pkgs is *manually moderated* — each `submit` opens a
  PR a human must review, so thurbox's per-`feat`/`fix`/`perf` cadence buried the
  maintainers (30 open PRs at once, flagged in
  [microsoft/winget-pkgs#405639](https://github.com/microsoft/winget-pkgs/pull/405639)).
  The throttle step ages our own last thurbox PR (via `gh pr list`, merged or
  open) — the winget analog of the choco feed's Published date; younger than the
  window ⇒ **skip the submission, exit green** with a `::warning::` (they
  coalesce into the next monthly PR). As second-line cleanup for a PR that still
  stacks (e.g. a manual dispatch inside the window), a follow-up `gh pr close`
  closes every older still-open `Thurbeen.thurbox` PR from the token account
  (wingetcreate's `--replace` only supersedes a *published* manifest version,
  not a pending PR; best-effort, never fails the release).
  The release zip is a `zip` installer with
  `NestedInstallerType = portable` (PATH aliases `thurbox`/`thurbox-cli`, no
  MSI). Install: `winget install Thurbeen.thurbox`. Windows x86_64 only.

See `packaging/README.md` for the full packaging overview.

### Commit Types and Versioning

Thurbox 1.0+ follows [Semantic Versioning](https://semver.org/):

- **feat**: Minor version bump (1.x.0)
- **fix, perf**: Patch version bump (1.0.x)
- **docs, chore, ci, style, test**: No release (appear in next version)
- **BREAKING CHANGE**: Major version bump (x.0.0)

A breaking change bumps the major version automatically via `cog bump --auto`
(at 1.x+; on a `0.x` line cocogitto maps breaking to a *minor* bump instead, so
the only way to cross into 1.0 was the explicit-version Release dispatch).

## Conventional Commits

All commits must follow
[Conventional Commits](https://www.conventionalcommits.org/).
Enforced by cocogitto via pre-commit hooks.

- **Types**: feat, fix, perf, refactor, docs, style, test,
  chore, ci, build, revert
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Agent Definitions

> Per-agent reference + the "adding a new built-in" checklist:
> `docs/AGENTS.md` (each built-in's exact config, ID model, and status-hook
> mechanism, plus every file to update when promoting a CLI to a built-in).

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`, `pi`, `omp`) on first run.
Each `[[agents]]` entry is an `AgentDef`:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = []                               # always passed; bake a model here if you want one
resume_args = ["--resume", "{id}"]      # emitted when resuming
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]  # emitted on a fresh spawn

[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]      # id-less: resumes the last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

Each `*_args` group is appended only when its driving value is
present, with `{id}` substituted; `args` is always passed. No
model is ever passed — each agent uses its own default config
(put `["--model", "opus"]` in `args` if you want to pin one).
A second token, `{home}`, expands (at spawn, on the spawn worker —
`session_ops::expand_home_in_def`, called from
`spawn::adapt_def_for_launch`, which both a fresh launch and
`session_ops::restart` go through) to the resolved home dir — the **remote** home for an
SSH/WSL host — so an agent that wants a session *file path* rather than a
bare id (the built-in `omp`, below) launches against a concrete,
quote-safe absolute path (a literal `~` would never expand — args are
POSIX-quoted).
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

**Session id pinning vs. `resume_latest`.** thurbox generates the
`agent_session_id` (a UUID) and `claude`/`pi` accept it at creation
(`--session-id {id}`), so only those two resume/fork by that exact id. The other
built-ins (`codex`, `opencode`, `antigravity`, `aider`, `copilot`) can't pin or
report their id, so they set `resume_latest = true` with **id-less** resume/fork
flags: the agent resolves "the last session in *this* directory" itself (`codex
resume --last`, `opencode --continue`, `agy --continue`, `aider
--restore-chat-history`, `copilot --continue`) — which works because restart
reuses the session's cwd and a single-repo fork reuses the parent's.
`resume_latest` only changes *when* the resume group fires
(`session_ops::resume_trigger_for`): for these agents restart always resumes; for
claude it defers to an on-disk transcript check. **`omp`** (Oh My Pi) is a third
kind: it generates its own internal id and won't take thurbox's, but its
`--session <path>` creates a fresh session at a missing path, so thurbox maps its
UUID to a deterministic file (`--session
{home}/.omp/agent/sessions/thurbox-{id}.jsonl` on create, `--resume` the same on
restart). Neither id-pinned nor `resume_latest`: `resume_trigger_for` resumes it
iff that JSONL exists (`session_file_template` — agent-neutral, keyed on a
`new_session_args` token that is a path *and* carries `{id}`, not on the agent
name); a remote-omp restart can't stat the host file from the UI thread, so it
starts fresh (documented fallback). Caveats: agents without `fork_args`
(`antigravity`, `aider`, `copilot`, `omp`) start fresh on `Ctrl+F`; and a
**multi-repo** fork of a cwd-scoped agent lands in a fresh symlink workspace, so
`--last`/`--continue` finds no parent session (multi-repo *restart* still resumes,
keeping the same workspace dir).

- **Data type**: `session::AgentDef` / `session::AgentRegistry`
  (`session/agent_def.rs`, pure data + substitution logic).
- **Loading**: `agent::agent_config::load_or_seed()` reads/seeds
  the TOML; `builtin_registry()` is the fallback.
- **Launching**: `agent::GenericProvider` wraps an `AgentDef` and
  implements the `AgentProvider` trait (`command()` +
  `build_args(&SessionConfig)`). It is constructed from the session's
  `AgentDef` at each launch site — `session_ops` for a spawn or restart,
  `kernel::terminal` when a pane adopts one.

A session stores only its **agent name**; there are no
per-session model/permission/prompt/tool knobs.

**Custom-agent status hooks (`hook_schema`).** thurbox stays agent-neutral, so
the built-in **hooks** extension wires status hooks only for the built-ins it
knows by name — a **custom** agent (e.g. a rebranded-claude `fleet`) gets no
`--settings` patch and so never reports working/blocked/done. An optional
`AgentDef.hook_schema: Option<String>` closes this: it names the hook **family**
the CLI speaks, and `agent::extension_config::apply_agent_patches` (+ its
uninstall reverse) fans each `[[agent_patches]]` out to the built-in named
`patch.name` **and** every agent with `hook_schema == patch.name`. So
`hook_schema = "claude"` on `fleet` injects the same `--settings {home}/
claude.json` claude gets — and, because the remote rewrite
(`session_ops::spawn::adapt_agent_args_for_remote`) keys off the `--settings`
arg rather than the agent name, remote/WSL wiring follows for free. Only the
per-arg-patch families (claude, aider) need it; codex/opencode/antigravity/vibe/
copilot wire through their own config dir, so a rebrand sharing that dir already
reports. thurbox bakes in no agent knowledge — the *user* asserts the family.

### Multi-repo sessions (symlink workspace)

A session can span several repositories (the repo picker allows multiple;
headless callers pass `--add-repo`/`--add-dir`, below). Because agent CLIs differ
wildly in how — or whether — they accept extra directories, thurbox passes **no**
per-agent `--add-dir`-style flags. Instead, a session with more than one member
directory launches in a per-session **symlink workspace**:
`~/.local/share/thurbox/workspaces/<agent_session_id>/` holds one symlink per repo
(worktree checkout or plain dir) and the agent starts there (`cwd` = the
workspace), so every agent sees each repo as a subdirectory — agent-neutral, no
`agents.toml` changes.

`SessionInfo.cwd` keeps the **primary** repo (display / editor / git context); the
workspace is a spawn-time process-cwd detail, derived idempotently on every launch
from the persisted members and never stored. `workspace::ensure_workspace` /
`remove_workspace` (`src/workspace.rs`) build and tear it down; the member set is
the single `App::session_member_dirs` list that also feeds the rendered repo
names, and `App::resolve_process_cwd` picks workspace-vs-primary. Single-repo
sessions are unchanged (`cwd` = the repo directly).

**Headless multi-repo.** The same multi-repo shape is reachable without the
TUI: `SpawnRequest.extra_repos: Vec<ExtraRepo>` (`session/automation.rs`) carries
each additional repo, where `ExtraRepo { repo_path, worktree: bool, base_branch }`
either gets its **own isolated worktree** on the spawn's shared `worktree_branch`
(off its own base — the per-repo-PR model flow uses) or is attached **as-is** as
an additional dir. `session_ops::spawn::resolve_dirs` builds the worktrees +
additional dirs and `resolve_launch_cwd` mirrors the TUI's `resolve_process_cwd`
(symlink workspace when ≥2 members). The CLI exposes it on `session create` and
`task create` via repeatable `--add-repo PATH[@BASE]` (worktree) and `--add-dir
PATH` (as-is); `AutomationAction::Spawn` persists the list as JSON in the
`action_extra_repos` column (schema v33, on both `tasks` and `automations`;
`NULL`/empty = single-repo, so old rows are byte-identical). The flow extension's
`create-task.sh` forwards these flags (see `extensions/flow/FLOW.md`).

## Remote SSH & WSL Sessions

Sessions can run on an **off-local host** while the TUI runs locally: a
**remote machine over SSH**, or a **local WSL distro** (`wsl.exe`). A WSL
distro is modeled as "SSH without the ssh" — the *only* difference is the
launch prefix (`wsl.exe -d <distro>` vs `ssh <dest>`); tmux, git, the agent,
and the worktrees all run **inside the distro** at native Linux paths, so
everything downstream of the launcher (control-mode protocol, POSIX quoting,
worktree layout) is identical to the SSH path — no `wslpath` translation. Hosts
are declared as data in `~/.config/thurbox/hosts.toml` (seeded commented-out;
fresh install = zero SSH hosts, behaves as before), **plus WSL distros are
auto-discovered on Windows** (`wsl.exe -l -q`) with no config. The seeded file
documents every field inline; the schema:

```toml
# An SSH host (the default kind):
[[hosts]]
name = "devbox"               # required — backend id "ssh:devbox"; what --host expects
destination = "me@devbox"     # required for ssh — target ("user@host" or ~/.ssh/config alias)
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
                              # optional (default []) — extra ssh flags; no ~ expansion, use abs paths
socket = "thurbox"            # optional (default "thurbox") — host `tmux -L` socket
session = "thurbox"           # optional (default "thurbox") — host tmux session name
worktrees_dir = "/home/me/.local/share/thurbox/worktrees"
                              # optional — abs worktrees dir on the host
multiplexer = "tmux"          # optional (default "tmux") — set "psmux" for a Windows SSH host

# A WSL distro (only needed to OVERRIDE auto-discovery, e.g. a custom worktrees_dir):
[[hosts]]
name = "ubuntu"               # → backend "wsl:ubuntu"; what --host expects
kind = "wsl"                  # required to select the WSL transport
distro = "Ubuntu-22.04"       # optional (default = name) — the wsl.exe distro name
```

Only `name` (+ `destination` for ssh, `kind` for wsl) is required; every other
field's default is in the comments above and in `docs/CONFIG.md`.

How it works: `TmuxBackend` is transport-neutral
(`agent::transport::TmuxTransport`). The local backend launches
`<mux> -L thurbox …`; an SSH backend launches `ssh <dest> <mux> -L thurbox …`;
a **WSL backend launches `wsl.exe -d <distro> tmux -L thurbox …`**
(`TmuxTransport::Wsl`). `wsl.exe` forwards whitespace-free tokens to the
in-distro shell like `ssh` does, so the same POSIX quoting
(`shell::posix_quote`) and the byte-identical control-mode protocol
(`control_mode.rs`) apply — only the one-time process launch differs. (An arg
*containing whitespace* is preserved as one word, so multi-word `sh -c` scripts
go through `wsl.exe --exec` instead — see `shell::wsl_command` /
`git::host_shell_c`.) The local `DEFAULT_MUX` is **`tmux` on
Linux/macOS and `psmux` on Windows** — psmux is a native-Windows, drop-in tmux
clone (ConPTY, no WSL) speaking the **same control-mode wire protocol** and
pane-id (`%N`) / `-L` socket model, so the whole backend is parameterized by
binary name rather than forked (a remote SSH host can also pin
`multiplexer = "psmux"`); a WSL distro runs `tmux` inside the distro. The
control-mode protocol is byte-identical over either transport/binary, with
**psmux divergences** (verified against psmux 3.3.6, each branched on
`TmuxTransport::uses_psmux()`) — psmux lacks `send-keys -H`, does not join
`new-window` trailing tokens or honour its `-e`, and implements no control-mode
paste command. So thurbox re-encodes keystrokes from the primitives psmux does
support (`send_keys_commands`), folds env + command into **one token** of
PowerShell (`psmux_window_powershell`), and routes a bracketed paste out of band
through the one-shot CLI `psmux send-paste` (`control_mode::PsmuxPaste`). Each
workaround has non-obvious quoting/tokenizing constraints — **read the psmux
divergences subsection of ADR-13 in `docs/ARCHITECTURE.md` before touching this
path**; delivery is probed by `scripts/dev/e2e/windows-vm.sh test` (probes C, D).

`multiplexer = "psmux"` also declares the **host is native Windows**
(`HostDef::is_windows` — the multiplexer is the proxy for the platform, since a
WSL distro runs `tmux` inside Linux), and a Windows host has no POSIX shell. So
each remote probe ships **two scripts emitting one line protocol** —
`git::host_probe` picks `sh -c` or `powershell -EncodedCommand`
(`host_powershell_c`; UTF-16LE base64, because ssh space-joins its args for a
default sshd shell that is commonly PowerShell and expands `$…` inside them) —
and `git::remote_home` resolves `%USERPROFILE%` rather than `$HOME`, which under
`cmd`/PowerShell prints the literal string and exits 0. Remote error *messages*
are cleaned in one place (`git::reportable_stderr`): OpenSSH ≥ 10's three-line
post-quantum advisory is dropped (it is informational, on stderr, and **first**,
so it used to be the whole reported error), and PowerShell's `#< CLIXML` stderr
envelope is decoded to the message inside it. See the two subsections after
"psmux divergences" in ADR-13.

Each host registers a backend named
`ssh:<name>` / `wsl:<name>` (`TmuxBackend::from_host`, registered lazily in
`main.rs` from `host_config::load_all_with_warnings`: discovery/down hosts must
not block startup, so `check_available`/`ensure_ready` are deferred to first use
— looking a backend up is a map read, and the blocking `ensure_ready` runs on the
attach worker in `kernel::terminal` (and on the spawn worker for a fresh
session), never on the loop, ADR-P12).

- **Data**: `session::HostDef` (with `kind: HostKind {Ssh, Wsl}`) /
  `HostRegistry` (pure data, in `session/` so both `agent` and `git` can use
  it); backend-name helpers `is_ssh_backend`/`is_wsl_backend`/
  `is_remote_backend`. **Loading**: `agent::host_config::load_all{,_with_warnings}`
  = configured hosts + `discover_wsl_hosts()` (deduped; a configured entry wins).
- **Selection**: `SessionConfig.backend` (`ssh:<host>` / `wsl:<distro>` or `None`
  = local). The TUI new-session flow shows a **host picker** first (skipped when
  none configured/discovered); the chosen host runs git worktree creation +
  branch listing on that host.
- **Worktrees**: `git::*_on(host, …)` variants run `git` via the host launcher
  (`git::host_launcher` → `ssh …` or `wsl.exe …`). Worktrees live under the
  host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved +
  cached per backend name — a WSL distro has no `destination`).
- **Persistence/restore**: `backend_type` round-trips in SQLite; restore
  discovers windows **per backend** so off-local sessions re-adopt against their
  own host. In v2 there is no separate restore pass: a session is adopted when a
  pane first asks to paint it, and readying its backend, discovering its window
  and attaching all happen on `kernel::terminal`'s attach worker — the sharpest
  teeth in the loop, since a down host runs out its ssh timeout. So an
  unreachable or slow host never blocks a frame, and nothing is readied that
  nothing is looking at (ADR-P7/ADR-P12, `docs/PERFORMANCE.md`).
- **Headless**: `thurbox-cli session create --host <name>` spawns on the host
  (an SSH name or an auto-discovered WSL distro name).
- **Agent config on the host**: agent args referencing thurbox-managed config
  by *local* path (the hooks extension's `--settings <config>/hooks/
  claude.json`) would kill the remote agent on launch ("Settings file not
  found"). `session_ops::spawn::adapt_def_for_launch` (shared by headless
  spawn and the TUI, run on the spawn worker — never the UI thread) rewrites
  them per host: on a POSIX remote the home-anchored path is **translated to
  the remote home**, the file copied there, and the arg substituted. A
  **Windows-local** config root (`C:\…` — the Windows TUI driving a WSL distro)
  has no absolute counterpart to mirror, so it lands under the remote
  `$HOME/.config/<root-name>` (final component = dev/release isolation), with
  `\` honoured as a separator **only** for such a root (it is a legal POSIX
  filename char) since the injected arg mixes them (`C:\…\hooks/claude.json`).
  On a psmux host (while `psmux_hook_rewrite_supported` stays off) or a failed
  home lookup/copy the **flag+path pair is stripped** so the agent launches
  clean — surfaced as a `Hooks: degraded` row in the info panel
  (`SessionInfo.hook_wiring`). Literal signal commands carried directly in
  args (aider's `--notifications-command`) are rewritten too.
  The local-path env hints
  (`THURBOX_METRICS_DIR`/`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`) are likewise
  skipped for remote spawns (`inject_thurbox_env`); only the opaque identity
  vars travel.
- **Remote session status** (hooks-driven, like local, **all agents**):
  `thurbox-cli session signal` can't work from a host (no CLI there; it would
  write the host's own DB), so hook commands are **rewritten**
  (`builtin_hooks::rewrite_hook_signals_for_target`) to set a tmux **pane user
  option** instead — `tmux set-option -p @thurbox_state <s>` needs no socket,
  pane id, or identity inside a pane (the psmux form bakes in
  `-L <socket>`). Delivery per agent: claude's hooks file travels via its
  `--settings` arg; agents wired through their **own config dir** (codex,
  antigravity, opencode, vibe, copilot) are provisioned at spawn time by
  `session_ops::remote_hooks::provision_agent_hooks_on_host` — the rewritten
  payload shipped into the host's agent config dir with the local installer's
  safety rules (`requires_dir` probe over ssh, prune-then-merge for shared JSON,
  managed-marker guard for standalone files, compare-before-write; cached per
  `(backend, agent)`, best-effort, never fails the spawn; remote **cleanup** is a
  documented leave-behind). The local TUI's persistent control-mode connection
  subscribes once per connection (`refresh-client -B
  'thurbox-status:%*:#{@thurbox_state}'`, armed in `ControlMode::start` so
  reconnects re-arm; tmux ≥ 3.2 = the existing floor) and receives
  `%subscription-changed` pushes (≤1/s); a **remote psmux** connection instead
  runs a 1 s **poller thread** (`list-panes -F` diffed by
  `control_mode::diff_polled_hook_states`) feeding the same queue — armed only
  behind the psmux gate below (a poll is an active per-second command, unlike the
  passive subscription, and a *local* psmux session signals via `thurbox-cli`).
  Both channels drain each tick via `App::drain_remote_hook_events` into the same
  `set_hook_state` columns local signals use — so Done→seen acknowledgment, OS
  notifications, and the stuck-`working` fallback are shared. Events are matched
  by **backend name + pane id** (pane ids collide across hosts), allow-listed
  (remote-controlled text), and deduped against the cache (a reconnect re-report
  must not resurrect an acknowledged `done`). Those live channels die with the
  TUI, so the headless **`automation tick`** (the 60 s heartbeat keeper) also
  polls each host with live remote sessions in the DB
  (`session_ops::remote_hooks::poll_remote_hook_states` — one-shot `list-panes
  -F`, allow-listed, diffed against the stored `hook_state`) and writes changes
  into the same columns, so remote status keeps flowing with the TUI closed at
  tick cadence. Remaining carve-out: the **whole psmux/Windows-host path** — hook
  provisioning, rewrite shipping, and the status poller — is gated off on one
  switch (`session::psmux_hook_rewrite_supported`) until the psmux behaviors are
  proven by `scripts/dev/e2e/windows-vm.sh test`'s probes; such sessions show a
  `Hooks: degraded` hint instead of silently idling.
- **Remote teardown** (WSL inherits the SSH path): `session delete --force`
  teardown is **backend-aware** — `teardown_runtime_resources` resolves the
  session's `HostDef` from its `backend_type` and, for a remote session, kills
  the pane via `kill_pane_remote(host, backend_id)` and removes each worktree
  via `git::remove_worktree_on(Some(host), …)` (local sessions keep the
  `kill_window`/`remove_worktree` + Windows pane-reap path). Best-effort: an
  unreachable host or a missing `hosts.toml` entry is recorded in
  `ForceDeleteReport.remote_teardown_error` (surfaced in the CLI JSON) and the
  row is still soft-/force-deleted. Like local force-delete it removes the
  worktree *directory* only, leaving the branch. `wsl.exe`'s exact arg-passing
  isn't verified in CI (no WSL runner); the construction is unit-tested
  (`transport::tests::wsl_*`, `git_command_wsl_*`).
- **Local e2e**: `scripts/dev/e2e/linux-container.sh up` spins a throwaway Podman
  container (sshd + tmux + git) and `… test` asserts a session lands on the
  `ssh:podman` backend (state under `target/`, never touches your real
  `~/.ssh`/`~/.config`).

## thurbox-cli

A second binary (`thurbox-cli`) drives the same SQLite-backed,
tmux-hosted sessions headlessly (no TUI). It shares the database
with the TUI; changes appear via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-cli
thurbox-cli session create --name demo --repo-path /path \
    --agent codex --worktree-branch feat/x
# Spawn on a remote host from hosts.toml (worktree + tmux live remotely):
thurbox-cli session create --name demo --repo-path /srv/repo \
    --host devbox --worktree-branch feat/x
# Spawn a worker under a lead session (parent must exist):
thurbox-cli session create --name worker --repo-path /path \
    --parent <lead-uuid>
# Multi-repo: each --add-repo gets its own worktree on --worktree-branch;
# --add-dir attaches a repo as-is (no branch). The agent launches in a
# symlink workspace gathering every repo. Works on `task create` too.
thurbox-cli session create --name demo --repo-path /a \
    --agent claude --worktree-branch feat/x \
    --add-repo /b@main --add-repo /c@master --add-dir /reference
thurbox-cli session list                       # human-readable table
thurbox-cli session list --json | jq           # machine output for scripts
thurbox-cli session list --parent <lead-uuid> --json | jq  # direct children only
```

Subcommands: `session` (create/list/get/delete/restore/restart/
send/capture/focus/signal), `automation` (alias `auto`:
create/list/show/edit/remove/run/runs/tick), `task` (alias `todo`:
create/list/show/edit/remove/run), `message` (alias `msg`:
send/inbox/prune — the inter-session mailbox queue; see below), `editor`
(get/set the Ctrl+O editor command; `editor mode <auto|terminal|gui>` chooses
how it launches — terminal editors get a real TTY via a tmux popup or TUI
suspend, GUI editors spawn detached; see the Editor Integration section of
`docs/FEATURES.md`), `config`
(validate/show — strict-parses every config file / prints the
effective resolved config; see `docs/CONFIG.md`), `extension`
(alias `ext`: install/uninstall/reinstall/list/available/update/activate/
deactivate/status — manage opt-in extensions; see below), `version`
(prints the running version; `--check` queries GitHub's latest release —
gated on `[features] version_check`, on by default for 1.0), `update`
(downloads, verifies, and replaces the installed binaries with the latest
release **within the current major** — a new major is reported, never installed,
because 2.x replaced the whole interface; `--force` bypasses the
up-to-date/dev-build/major guards; gated on
`[features] auto_update`, on by default for 1.0; the TUI also runs this silently on
startup when the flag is on), `notify`
(diagnose OS desktop notifications: prints the detected delivery backend
and last error; `--test` fires a sample — see OS notifications below), `perf`
(print the perf snapshot a running TUI publishes while `THURBOX_PERF_LOG`
or its perf HUD is active — see `docs/PERFORMANCE.md`), `plugin`
(v2 interface plugins without a TTY: `dir` reports the directory in force and
which of the two rules chose it, `new <name>` writes a starter that already
loads, `check` loads the interface the way `thurbox` does and exits non-zero on
a failure — **including on a pane that loaded but which no arrangement places**,
printing the `layout.lua` line to add — `list` is the same inventory the settings
modal's Interface tab shows, and
`install|sync|update|remove|available` manage panes from a declarative spec
— see `docs/PLUGINS.md`).
Output is
**human-readable by default** and switches to JSON automatically when stdout is
piped (so `… | jq` keeps working); force a format with `--json` (compact),
`--pretty` (indented JSON), or `--text` (human even when piped).

`session delete <uuid>` **soft-deletes** by default — only the DB row is marked
deleted (the TUI tears down the tmux window/worktree on its next sync), and
`session restore` revives it. `--force`
(`session_ops::delete_session_headless`) also kills the tmux window, removes
worktrees + the symlink workspace, and disables `send` automations targeting the
session — for headless cleanup with no TUI running. Teardown is best-effort
(failures land in the JSON report); the row is always soft-deleted last. A
`--force` delete stamps `sessions.force_deleted` (schema v37): the row still
appears in the restore list **tagged `force-deleted`** and is restorable
**best-effort** — force-delete removes the worktree *directory* but not the git
branch, so restore reattaches each surviving branch's committed work
(`App::recreate_worktrees`); only uncommitted/untracked changes are gone. Because
that recovery is lossy, the headless `session restore` **refuses a force-deleted
row unless `--best-effort`** (its JSON then carries `best_effort: true`).
`restore_session` clears both `deleted_at` and `force_deleted`.

The **TUI** `Ctrl+D` soft-deletes too (with a `Ctrl+Z` undo window). The
`[features] soft_delete` flag (default `true`) governs only this TUI path: set it
`false` and `Ctrl+D` becomes a hard delete — the same
`delete_session_headless(.., force=true)` teardown — since there is no `Ctrl+Z`
for it. That hard delete is **conditional**: a confirmation appears **only when
the session has work at risk** — uncommitted/untracked files, unpushed commits, a
multi-worktree session whose other checkouts the snapshot does not stat, or a
state that can't be read at all (remote host / git error → confirm to be safe) —
itemizing what would be lost; a known-clean session is deleted with no prompt.
The assessment is the pane's (`at_risk` in `ui/plugins/10_sessions.lua`, reading
the snapshot's `git` stats — v1 computed it in Rust over `git::worktree_stats`),
and the question travels through the shared `store.confirm` to the confirmation
float (`ui/plugins/60_confirm.lua`) rather than a bespoke modal. `Ctrl+U` lists the deleted rows (`ui/plugins/80_restore.lua`,
a float) and `Enter` restores the one under the cursor; a **force-deleted** row
asks first — through the shared `store.confirm` question, not a bespoke modal —
since recovery is committed-state-only, and only then issues `restore` with
`best_effort`. The flag never changes
`thurbox-cli session delete`, which stays soft unless `--force`.

### Parent sessions (lead/worker)

Sessions carry an optional **`parent_session_id`** so orchestration scripts can
model lead → worker relationships. `session create --parent <uuid>` sets it (the
parent must be an existing active session — validated before any side effects);
`session list`/`get` emit it in the JSON (`null` for top-level) and `session list
--parent <uuid>` filters to direct children. The link is **purely
informational**: deleting a parent never cascades (orphans render as top-level),
and the parent is only validated at creation. In the TUI, **`Ctrl+F` fork**
records the source session as the fork's parent; the session list nests children
under their parent **within the same repo group** (muted `└` tree prefix; a child
whose parent renders in another group keeps its own position with a `↳` mark).
The nesting lives in `ui/plugins/10_sessions.lua` (`build_model`, which walks the
snapshot's rows into depths — a port of v1's `compute_session_order`), so
`Ctrl+J`/`Ctrl+K` navigation follows the tree automatically. A `Parent:` row is
the out-of-tree
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) plugin's,
not the bundled interface's. Storage: nullable `sessions.parent_session_id`
(schema v30; v29 is reserved by an in-flight branch).

### Manual session ordering

The session list is **manually orderable**: `Shift+J`/`Shift+K` (session list
focused; rebindable `SessionListMoveDown`/`SessionListMoveUp`) move the selected
session one row down/up. Manual order **wins** — status changes only recolor the
dot, never move a row. A move swaps two adjacent *blocks* (a row plus its nested
children, so a parent drags its subtree): root rows swap within their repo group,
the **whole group** swaps past a group edge, and nested children move among their
siblings only. It is computed in the pane (`10_sessions.lua`'s `move`,
`root_ranges` and `child_ranges` — ports of v1's `move_in_order`) over the items
it actually rendered, and the result is handed back whole as one
`Command::Order { list }`: the kernel densely renumbers all sessions `0..n` and
persists, so the order survives restarts and syncs across instances via
`data_version` polling. Storage: nullable `sessions.display_order` (schema v31);
`None` = never moved, renders after ordered sessions in creation order (new
sessions append to their group). **`Shift+S`** (rebindable
`SessionListSortAlphabetically`) sorts by name **within each repo group** in one
shot, preserving group order (still by lowest `display_order`) and parent/child
nesting, and issues the same `Order` command (v1's
`sort_alphabetically_within_groups`).

### Inter-session messages (mailbox queue)

A general, agent-neutral **message queue** lets one session hand another a
**structured payload** without scraping its rendered terminal — the channel
extensions use for agent↔agent coordination (flow's clarify→plan→build relay is
the first consumer). A message is addressed **to** a session and carries a
free-form `kind` tag (`questions`/`plan`/`result`/… are conventions, not an enum),
a `body`, and optional provenance. Storage is the `session_messages` table (schema
**v32**, CRUD in `storage/messages.rs`); `Database::claim_messages` is a single
`UPDATE … RETURNING`, so the TUI, a cron tick, and a wake nudge can drain
concurrently without double-processing.

- **Identity (the registry key, self-knowable).** A session's `SessionId` is
  **stable for life** — `respawn_stale_session` reuses the original id on
  re-adoption (no soft-delete + new-row churn), so a cached id or queued message
  never goes stale. At spawn thurbox injects `THURBOX_SESSION` (= the `SessionId`,
  threaded via `SessionConfig.session_id` so it's known *before* launch and reused
  on respawn) and, for task-spawned sessions, `THURBOX_TASK` (= the task id) —
  both distinct from the older `THURBOX_SESSION_ID` (= `agent_session_id`, read by
  the metrics statusline). So a `thurbox-cli` call *inside* a session proves its
  own identity without scraping panes or names.
- **Consequence for the CLI surface**: an agent passes **no ids**. `message send
  --to <uuid|name>` stamps provenance from the injected identity, `message reply
  <message_id>` routes back to that message's sender (the replier never learns a
  peer's session id), and `message inbox [--claim]` defaults `--for` to the
  calling session. A send/reply with a wake also arms the automation heartbeat so
  a missed wake is still drained headless.

**Full flag list, the body/kind limits, backpressure cap, and retention/pruning
are in the Inter-Session Messages section of `docs/FEATURES.md`.**

An automation's `AutomationAction` is one of: **Send** (paste a prompt into a
running session), **Spawn** (start a fresh session and prompt it), or **Exec**
(run a shell command headlessly — `sh -c`, or `cmd /C` on Windows — with no
agent/session; its exit status + tail-truncated output land in the run history).
`Exec` is the deterministic-scheduled-job action (the task-integration sync
extensions use it). The runner is `session_ops::run_exec_command`, which blocks
until the child exits and is called from exactly one place —
`cli::automations`'s `tick`. Firing is **CLI-only in v2**: the interface neither
runs schedules nor holds a worker for them, so there is no in-flight/`skipped`
bookkeeping in the binary that draws the screen. The command is stored in the
`action_command` column (schema **v36**, on both `tasks` and `automations`).
Author one headlessly with `thurbox-cli automation create --command "<shell>"`
(mutually exclusive with `--session`/`--repo`), or from an extension manifest
(`[[automations]]` with a `command` field instead of `session_ref`/`prompt`).
`Task.action` shares the enum but tasks never carry an `Exec`
(it's automation-only).

Automations fire even when the TUI is closed: a tmux heartbeat
keeper window (`automation-heartbeat`, armed on TUI startup and on
`automation create`) loops `automation tick` every 60 s and keeps
the tmux server alive. `packaging/` ships opt-in systemd/launchd
units for reboot-proof firing. Concurrent firers are de-duplicated
by `Database::claim_due_automation` (atomic CAS), so the keeper,
an OS timer and a hand-run `tick` never double-fire.

**No automations pane.** The interface has none, and `[features] automations`
no longer hides one: its only *effect* in the TUI is gating **arming the tmux
heartbeat keeper** at startup (`src/main.rs`; it also rides the live-reload merge
as a restart-only flag and is published to Lua in `thurbox.features`). The rows
are published as `thurbox.automations` and the kernel accepts an `automation`
command (enable/disable/run/delete — `run_now` only marks it due, so the tick
stays the one execution path), so a pane is a plugin somebody can write: both
halves are done and nothing in `ui/` uses them yet.

> A pane is owed, and it is Tier 2 in `openspec/changes/v2-parity-gaps/` beside
> the tasks one. v1's shape — a pane under the session list, sharing one circular
> `j`/`k` list with it, with the centre pane as its editor and the run history
> below — is in that ledger and on the `v1.x` branch; it is deliberately not
> described here as if it existed.

## Tasks (todo list)

Todo items (title + markdown description + status), **CLI-only**: the interface has
no tasks pane. The data, the storage and the agent linkage are unchanged, so
extensions and scripts that used them still work.

A task can be **acted on by a coding agent**: `Task::agent_prompt()` builds an
`id + # title + markdown description` block plus self-service hints (`thurbox-cli
task show <id>` to read the record, `thurbox-cli task edit <id> --status done` to
close it), and `task run` sends or spawns. Triggering advances `Todo → InProgress`.

- **Data** (`session/task.rs`): `Task` (`id`, `title`, `description:
  Option<String>`, `status: TaskStatus` {`Todo`/`InProgress`/`Done`}, `action:
  Option<AutomationAction>`, plus `source`/`external_id`/`external_url` for
  tracker sync — `source = "local"` for native todos, a tracker tag for imported
  ones. `(source, external_id)` is the dedup key.
- **Storage** (`storage/tasks.rs`, schema v25/v26): `tasks` mirroring the automation
  action columns plus a nullable `description`, soft-delete via `deleted_at`,
  audited under `EntityType::Task`, `idx_tasks_external` on `(source,
  external_id)` (v35) backing the upsert lookup.
- **CLI**: `thurbox-cli task` (alias `todo`) —
  `create`/`list`/`show`/`edit`/`remove`/`run`, with `--description` (markdown) and
  the external-sync flags. `[features] tasks` is **accepted and ignored** — nothing
  reads it, in the CLI or anywhere else.

> A pane is owed. It is Tier 2 in `openspec/changes/v2-parity-gaps/`, and the shape
> a plugin would take is the same one `10_sessions.lua` uses: read the snapshot,
> return a tree, send a command.

## Extensions

`extensions/` holds opt-in, **agent-agnostic** add-ons that build on
`thurbox-cli` without touching the core binary. Each ships an
`extension.toml` manifest installed via `thurbox-cli extension install
<name>` (with a thin curl-able `install.sh` shim over it).

- **`extensions/flow/`** *(experimental — new and under active testing)* — a
  focus-protecting triage agent: brain-dumps become thurbox tasks, dispatchable
  ones spawn worker sessions (on `flow/<slug>` worktree branches, agents
  `flow-worker`/`flow-worker-heavy` mapped in `agents.toml` to any CLI), a
  dedicated `flow` session monitors them, and every reply ends with the single
  next thing to focus on. Dispatch is **plan-first**: `scripts/create-task.sh`
  owns the worker prompt and injects a mandatory clarify → plan → build phase (≥3
  clarifying questions, then a written plan gated on user approval, then
  implement; seeded from `--accept`) so each worker plans before it codes. A dump
  spanning several `repos.md` repos becomes one **multi-repo** task:
  `create-task.sh` forwards `--add-repo PATH@origin/<base>` (own isolated
  worktree per repo) / `--add-dir PATH` to `task create`, and the worker opens a
  **separate PR per repo it changes** (its `result` carries `pr_urls`).
  Worker↔flow coordination is **event-driven over the
  [inter-session message queue](#inter-session-messages-mailbox-queue)**: a
  worker pushes `message send --to flow --kind questions|plan|result` (waking
  flow) with **no ids** (thurbox stamps sender + task from the injected
  `THURBOX_SESSION`/`THURBOX_TASK`); flow drains its inbox (`message inbox
  --claim`), surfaces the questions/plan under "Needs you", and relays the user's
  answer with `message reply <message_id>` — routed to that message's sender, so
  flow never maps a task to a session id (`flow-snapshot.sh` name-parsing is now
  human-board only). The worker drains its own inbox on the resulting `inbox`
  wake. Flow ships **no scheduled automation** — a **manual** `tick` is the
  janitor/safety-net (drain missed wakes, reset stale tasks, dispatch). The
  behavior spec is `FLOW.md`, surfaced to whichever CLI runs it via context-file
  symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). See
  `extensions/flow/README.md`.
- **`extensions/forge/`** *(experimental)* — a workflow analyst that mines
  your tasks/sessions/automations (and their run history) for **recurring
  patterns** and writes ready-to-apply `thurbox-cli automation` proposals. It
  **proposes, never imposes**: a scan (driven by a weekly `forge-scan`
  automation on the `forge` session) only reads state and writes
  `proposals.jsonl` (rendered to `proposals.md`); nothing is created until you
  `apply <slug>` — and `proposals.sh apply` refuses any command not starting
  with `thurbox-cli`. Spec: `FORGE.md`.
- **`extensions/ci-shepherd/`** *(experimental)* — watches your open change
  requests (GitHub PRs / GitLab MRs / Bitbucket PRs; repos in `repos.md`) and
  dispatches a `shepherd-worker` fixer for each one with **failing CI**, a
  **changes-requested review**, or a branch that is **behind its target**
  (needs rebase — the normalized `rebase` signal from `provider.sh`, surfaced
  as the `REBASE` action flag by `scripts/classify.sh`; `dispatch-fix.sh
  --rebase` makes the worker rebase onto the base and force-push before fixing).
  When **several PRs in one repo** are all REBASE-only, `classify.sh`
  **serializes** them — only the lowest-numbered keeps the live `REBASE` flag,
  the rest become `REBASE-QUEUED (behind #n)` — so the shepherd rebases one at a
  time (each merge advances the base for the next), clearing the stack in O(n)
  rebases instead of the O(n²) of force-pushing N mutually-invalidating branches.
  A `shepherd` session monitors via a `shepherd-tick` automation; fixers are
  thurbox **tasks** (`fix #<n>: …`) that self-report with the same `===RESULT===`
  sentinel as flow. It is **forge-agnostic**: only **git** is baked in; *how* to
  talk to a repo's host is decided by the shepherd agent each tick — built-in
  **fast paths** (github `gh`/gitlab `glab`/bitbucket REST via
  `scripts/provider.sh`) plus an **agent-driven** path for any other forge
  (`provider.sh describe` hands the agent the remote + installed clients; it
  lists the repo itself and passes `--branch`/`--checkout-cmd`/`--feedback-cmd`/
  `--comment-cmd` to `dispatch-fix.sh`). Because thurbox's `--worktree` always
  runs `git worktree add -b` (which fails on an existing branch),
  `dispatch-fix.sh` adopts the request branch itself into a shepherd-owned
  worktree. It is also **session-aware**: the snapshot joins each request's head
  branch against the live `thurbox-cli session list` (`scripts/link-sessions.sh`,
  pure + bats-tested). A request whose branch already has a **non-fixer** thurbox
  session (someone working it by hand) is **not** dispatched (two worktrees would
  force-push the same branch) but is **monitored and folded into the merge
  ordering** — that live session counts as the repo's active worker, so the other
  same-repo requests queue behind it. While such a request stays actionable the
  shepherd **nudges the live session** over the message queue (`thurbox-cli
  message send`) to do the rebase/merge — once per pending ask (guarded by
  peeking its unread inbox), not every tick — so the slot actually clears.
  Spec: `SHEPHERD.md`.
- **`extensions/renovate/`** *(experimental)* — keeps local repos on up-to-date
  dependencies. A `renovate` session sweeps a `repos.md` watch list on a weekly
  `renovate-tick` automation and dispatches a `renovate-worker` per eligible
  repo; the worker runs **Renovate's `local` platform only**
  (`scripts/renovate-run.sh` hard-codes `--platform=local` — no hosted bot, no
  token, no Renovate-opened PR), tests the result, commits to a fresh
  `renovate/updates-<ts>` branch, and opens a review PR. Updaters are thurbox
  **tasks** (`update <repo> deps …`) that self-report with the same
  `===RESULT===` sentinel as flow. Unlike ci-shepherd it starts a *new* branch,
  so `scripts/dispatch-update.sh` uses thurbox's native `--worktree` (no branch
  adoption). Version strategy is per-repo (`strategy` column: `patch`/`minor`/
  `major`/`all`, layered as a `RENOVATE_CONFIG` overlay) plus a global
  `renovate-config.json`. Spec: `RENOVATE.md`.
- **`extensions/ui-skill/`** *(built-in, on by default)* — the odd one out: it
  ships no session, no automation and no agent. It installs a single **agent
  skill**, `thurbox-ui`, into each coding CLI's *personal* skill directory
  (`~/.claude/skills/`, `~/.codex/skills/`, `~/.config/opencode/skills/`,
  `~/.copilot/skills/`, `~/.agents/skills/` — each guarded by `requires_dir`, so
  a CLI the user does not have is skipped), so an agent in **any** session knows
  how to change thurbox's own interface. It replaces the workaround of attaching
  the interface directory to every session as an extra repo: a skill loads only
  when the request is about the TUI, where an extra repo is in front of the agent
  always. Like `hooks` it is **embedded + auto-activated** (see below) — for the
  same reason: someone who does not already know the interface is editable will
  not go looking for the extension that says so. Opt out with `thurbox-cli
  extension deactivate ui-skill`. The payload is one `SKILL.md` — the short form
  of `ui/AGENTS.md` + `ui/README.md` — and it hard-codes no paths, opening with
  `thurbox-cli plugin dir` so one file is correct for a release build, a dev
  build and a `THURBOX_UI_DIR` override alike. Delivery is the ordinary
  `[[external_files]]` machinery, marker-guarded: `install`/`update`/`uninstall`
  act on thurbox's own copies and leave one the user has taken ownership of alone
  (drop the `Managed by` line and it is theirs), while `reinstall` and `install
  --force` overwrite as they do everywhere else.
> **Removed.** Four per-provider task-integration extensions
> (`github-issues`, `gitlab-issues`, `linear`, `jira`) lived here and were deleted:
> four near-identical trees, each carrying a provider's API shape, for a job that is
> a `curl` and an `upsert`. What made them possible is still in the binary and is
> deliberately provider-neutral (ADR-20 — no provider name in the binary): the
> `task --source/--external-id/--external-url` flags, `get_task_by_external_id`,
> the `idx_tasks_external` index, and the `Exec` automation action. A scheduled
> `Exec` running a script of your own does what they did.

### Extension manifests + self-heal (`thurbox-cli extension`)

Extensions stay **data, not binary** (ADR-20): core thurbox knows a declarative
**manifest format**, never a specific extension. Each extension ships an
`extension.toml` (`session::ExtensionDef`, pure data in
`session/extension_def.rs`; loaded by `agent::extension_config`) with two halves:
an **install** spec (`home`, `[[agents]]` to register in agents.toml, `[[files]]`
payload, `[[symlinks]]`, `[[external_files]]`, `[[agent_patches]]`,
`[[config_merges]]`) and a **runtime** spec (`[[sessions]]` + `[[automations]]` to
ensure/self-heal). The `{home}` token expands to the resolved home dir.

Three of those reach **outside** the extension home, all reversible:
`[[external_files]]` drops a managed file into an agent's own config dir (guarded
by `requires_dir`), `[[agent_patches]]` appends args to an existing agent in
agents.toml, and `[[config_merges]]` deep-merges shipped JSON into an agent's
*shared* config file (`agent::json_merge`; uninstall prunes by the
`thurbox-cli session signal` marker, so removal survives payload schema changes).

**Built-in extensions** (`session_ops::builtin`) — two of them, `hooks`
(`extensions/hooks/`) and `ui-skill` (`extensions/ui-skill/`), which unlike user
extensions ship **embedded** in the binary and are **auto-activated by default**
(`ensure_builtin_extensions` at TUI startup + headless tick). Each is a
`Builtin` — embedded assets, a home under *this build's* config dir, and how it
describes what it just did — and the shared `Builtin::ensure` materializes the
assets locally and installs them through the ordinary machinery above, so a
built-in is not a second installer with its own bugs. They exist for the same
reason: what they wire up has to be there before the user knows to ask for it.
`hooks` gives the default agent's status hook zero-setup, and `ui-skill` gives
whichever coding CLI the user runs the knowledge of how to edit the interface.
**Which hook delivery mechanism each built-in *agent* gets (and the exact states
each can report) is documented per agent in `docs/AGENTS.md` → "Status hook
mechanisms"** — that is the reference to update when adding an agent.
Remote sessions are provisioned by
`session_ops::remote_hooks::provision_agent_hooks_on_host`; a psmux/Windows host
is gated off (`session::psmux_hook_rewrite_supported`) and shows `Hooks:
degraded`. Opt out of either with `thurbox-cli extension deactivate <name>` (records a
`builtin_<name>_optout` metadata flag so self-heal won't resurrect it — the key
format is chosen so `hooks` keeps producing the `builtin_hooks_optout` row it
wrote before there was more than one built-in); `activate`/`install <name>`
clears it.

`thurbox-cli extension` (alias `ext`) — `install <name|url|dir>` / `uninstall` /
`reinstall` / `list` / `available` (alias `search`) / `update [--all] [--force]` /
`activate` / `deactivate` / `status`. A bare name resolves to the official source
**pinned to the binary's release tag**, so a fetched extension matches the binary.

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every active
extension at TUI startup (before session restore) and at the top of the headless
`automation tick`. Consequence worth knowing before debugging a "zombie" session:
while an extension is active, deleting its session/automation is a **no-op** —
they are recreated. `extension deactivate` is the real off-switch, and headless
healing needs `[features] automations = true`.

**Installer resolution order, payload flags, versioning/staleness
(`installed_with`/`is_stale`), and the full self-heal contract are in ADR-21 of
`docs/ARCHITECTURE.md`.**

## Global search

`ui/plugins/65_search.lua` — a full-width strip above the chrome bands (v1 floated
it). It searches sessions by name, agent, branch and repo, **and by the text on
their screens**, which is the half that finds a session by the error in it.

- **Matching**: subsequence via `ui/lib/fuzzy.lua`, shared with the session list so
  the two cannot disagree. Screen text is matched as a **substring**, not a
  subsequence — fuzzy over a whole screen matches nearly everything — and is
  skipped for a session whose metadata already matched.
- **Terminal text is a *want***: the pane leaves its query in `store` under
  `want_content` and the kernel serves `thurbox.content` only while it is asking, so
  no interface pays for every agent's screen on every frame
  (`kernel::terminal::WANT_CONTENT`, capped at `CONTENT_LINE_CAP` = 500 lines, the
  same bound v1 used).
- **Highlighting is in place**: matches highlight *inside* the panes being searched
  and non-matching rows dim, rather than being reprinted in the strip. Moving the
  selection previews it in the owning pane; `Esc` puts back what you were looking
  at.
- Sessions is the only scope with a pane today. A result carries the pane it
  belongs to, so a returning surface is a scope added and nothing else changed.
- One deliberate divergence, recorded in `tests/v2_parity.rs`'s successor notes: v1
  also took `Ctrl+P`/`Ctrl+N` inside the strip because its search focus captured
  input ahead of the keybinding table. Here every chord goes through one registry
  where a plugin-scoped claim does not outrank a global one, so declaring them
  would take `Ctrl+N` from new-session everywhere.

## Session status (hooks-driven)

The session list shows, at a glance, which agents are blocked, working,
or done. `SessionStatus` (`src/session/mod.rs`) has six states — five driven by
**agent hooks**, not heuristics, plus `Unreachable` for a down remote host:

| State | Colour | Glyph | Meaning |
|-------|--------|-------|---------|
| `Working` | yellow | animated braille spinner (`⠋⠙⠹…`; static `◐`) | agent is actively running |
| `Blocked` | red | `◆` | agent needs input or approval |
| `Done` | blue | `●` (filled) | a turn just finished; shown until you switch away |
| `Idle` | green | `○` (hollow) | acknowledged (you moved off a Done), never active, or at rest |
| `Error` | red | `✗` | reserved for a crashed agent — **not derived yet** (no exit-code signal; exited → `Idle`) |
| `Unreachable` | muted grey | `⊘` | remote host down/offline; the ordinary row, derived from a live attach failure, awaiting reconnect |

**Unreachable sessions.** A persisted **remote** session whose host cannot be
reached **always appears in the list**, tagged `Unreachable`, rather than
silently vanishing. v2 gets there by derivation rather than by a synthetic row:
`kernel::terminal` records the attach failure for that session, and
`snapshot::with_reachability` folds "a remote backend plus a live attach error"
into the published status. So the row is the ordinary row — no second `Session`
kind, no dead input channel to guard, and nothing on the loop that can block on
ssh. (v1 inserted a `Session::placeholder`; the constructor is still in
`src/agent/backend.rs` and has no caller.)

The attach worker owns the retry: the same failed attempt is left alone for
`ATTACH_RETRY_INTERVAL` (20 s) and then made again, so a host that was offline at
startup recovers on its own instead of staying dead for the life of the process.
Recovery replaces nothing — the next attach simply succeeds and the derived
status goes back to what the hooks say.

The same treatment covers **mid-session host loss**: `drop_lost_panes` (per tick)
spots a *live* remote session whose pane is gone, lets it go, and clears the
readied-backend cache — the connection that session died with is the one every
other session on that host shares. The reliable signal is `has_exited()`: with
`remain-on-exit=on` a clean agent exit keeps its pane alive (no reader EOF), so a
remote reader hitting EOF means the host/SSH connection dropped. This composes
with the fail-fast SSH hardening (`crate::shell::SSH_HARDENING_OPTS` =
`BatchMode=yes` + `ConnectTimeout` + `ServerAlive*`), which stops a broken host
from prompting for a password on the TUI's terminal or hanging the render loop.

The live session list **animates** the `Working` spinner. The frames are
`theme.spinner` in `ui/lib/theme.lua` and the pane picks one from the elapsed
time it is handed (`status_glyph` in `10_sessions.lua`); the clock behind that is
the kernel's shared **animation tick** (`kernel::host::ANIMATION_HZ` = 8), which
the loop advances **only while something is actually animating** — a free-running
one invalidated every `pure` pane on every idle frame (ADR-P16). The filled `●`
(Done) vs hollow `○` (Idle) pair reads done-vs-seen at a glance;
`SessionStatus::icon()` is the static glyph, for contexts with no clock.

- **The callback.** Agents report transitions with
  `thurbox-cli session signal --state <working|blocked|done|idle>`
  (`cli::sessions::Action::Signal`). Identity is the injected
  `THURBOX_SESSION` (falling back to a lookup by `agent_session_id` /
  `THURBOX_SESSION_ID`), so a hook passes no id. It writes the persisted
  state and the TUI picks it up via `PRAGMA data_version` — works headless.
  (`idle` = agent at rest, e.g. a boot-time hook; `done` = a turn just finished.)
  **Remote** sessions use a different callback with the same downstream:
  the materialized hook file sets a tmux pane user option instead, delivered
  over the control-mode subscription into the same hook columns (see the
  Remote-session-status bullet in the Remote SSH & WSL section).
- **Persistence.** `sessions.hook_state` / `hook_state_at` / `seen_at`
  (schema **v34**), with targeted-UPDATE accessors `set_hook_state` /
  `mark_session_seen` / `load_hook_states` (`storage/sessions.rs`).
  `upsert_session` deliberately **never** lists the hook columns, so the
  TUI's full-row write-back can't clobber a state a headless hook set. A
  fresh spawn seeds **nothing** — a never-reported session is `Idle`, and the
  agent's hooks drive it from there (so an idle, just-booted agent doesn't look
  stuck working).
- **Derivation.** The snapshot carries each session's `hook_state` and folds
  attach state into the published status — a *remote* session with no live pane
  → `Unreachable` (`with_reachability`); a `working` one gone quiet → `Idle`
  (the fallback below, which subsumes exited → `Idle`); else the persisted state
  (`working`/`blocked`; `idle`/none → `Idle`). A local session is never
  unreachable: this is its machine, and a missing pane there means the agent was
  not launched. The rows are read on the snapshot's own schedule rather than per
  frame, gated on `PRAGMA data_version` moving (see `docs/PERFORMANCE.md`
  ADR-P6) — but the quiescence fallback is re-derived every tick, since output
  moves between reads. `done` shows as `Done` (blue)
  **whether focused or not** — so a turn you're watching visibly completes — and
  becomes `Idle` only when you **move focus off it** (acknowledge it): the focus
  change vs. `last_active_session_id` marks the just-left `done` session `seen`
  (persists `seen_at`, one-shot). A single focused session therefore reads
  `working ↔ done`; `idle` is the at-rest/acknowledged state.
- **Stuck-`working` fallback.** Hooks can miss the turn-end edge: Claude Code
  fires **no hook on interrupt** (Esc/Ctrl+C) nor when it returns to the idle
  prompt, so an interrupted (or crashed) turn would leave `hook_state = working`
  forever. `snapshot::with_output_quiescence` guards with an **output-quiescence
  fallback** (`WORKING_QUIET_MS`, 10 s): a `working` session with no terminal
  output for that long is treated as `Idle`, and so is one with no live pane —
  which is where v1's exited → `Idle` branch lands, since a pane whose stream
  ended leaves the live set. TUI agents animate their progress line (Claude's
  `(Xs · esc to interrupt)` ticks every second) so a genuinely-live turn never
  trips it; only `working` is time-gated. The signal is **terminal output, never
  the age of the hook state**: keyed on `hook_state_at` instead, every turn
  reports itself finished ten seconds in and starts again at the next hook — a
  spinner that stops early and restarts. Applied by `SnapshotStore::
  apply_output_quiescence` on the loop's own tick rather than in `refresh`
  (whose cadence is the database's) and re-derived from `hook_state` each pass,
  so it reverses itself when output resumes. The DB row is left untouched — the
  override is purely per-tick derivation.
- **Per-session only.** Status renders on the session's own row (and in the
  ` Sessions ` panel border title, one dot per session). Repo-group headers
  (`group_header_line` in `10_sessions.lua`) carry **no** status — a rolled-up
  group dot would restate what every member row shows. Status only recolors — it
  **never** reorders rows (the order is status-independent).
- **Colours** are tunable theme fields: `status_working` / `status_blocked`
  / `status_done` / `status_idle` / `status_error`
  (`session::theme_config`, all 36 presets + custom-theme overrides), published
  to Lua as theme roles and read by the pane.
- **Wiring the hooks** is the job of the built-in **hooks extension**
  (auto-activated; see the Extensions section) — core thurbox only knows
  the generic `session signal` command.

## OS notifications

When a session goes to `SessionStatus::Blocked` (the agent needs you, reported by a
hook) thurbox fires an OS desktop notification. `kernel::notify` owns the
per-session bookkeeping and the edge detection; `src/notifications.rs` is still the
leaf side-effect layer that knows only `session` + `paths`.

The trigger is the block edge by default; `also_on_waiting` extends it to
`Working → Done`. Observed once per tick in the same place status is derived, so the
rule cannot drift from the dot in the list. Deduped per session by
`min_interval_secs`, skipped when the session is the focused one
(`suppress_for_active`), body bounded to 200 chars so a huge OSC message cannot
overflow the banner.

- **Backend** (auto-detected): `notifications::detect_backend` resolves
  `[notifications] backend` plus host probing into `Dbus` / `WindowsToast` /
  `Macos` / `None` via the pure, table-driven `resolve_backend`. `auto` picks dbus on
  a Linux desktop, the Windows toast path on native Windows and under WSL when no
  dbus answers (`/proc/version` carries the Microsoft marker and `powershell.exe`
  is on PATH), the macOS banner otherwise. Delivery errors land in a process-wide
  `LAST_ERROR` and are surfaced by the diagnostic — under WSL the dbus path used to
  fail on connect and only `warn!`, so the user saw nothing.
- **Diagnostic**: `thurbox-cli notify` prints the detected backend, whether it can
  deliver, click-to-focus support and the last error; `--test` fires a sample
  *synchronously* (`send_blocking`, since the short-lived CLI has no dispatcher).
- **Click-to-focus** (dbus + macOS `terminal-notifier`): the callback writes a
  session id to the `metadata` row keyed by `PENDING_FOCUS_SESSION_ID_KEY`, and the
  loop reads-and-deletes it atomically (`take_pending_focus_session_id`, one
  `DELETE … RETURNING`) then focuses that session. The Windows toast and macOS
  `osascript` fallback show the banner but ignore clicks. **Raising the terminal
  window is deliberately not implemented** — thurbox runs inside an emulator it does
  not own, and per-emulator window control is fragile, especially on Wayland.
- **Gated by `[features] notifications`** (default on); knobs in `[notifications]`.
  `backend = "off"` is a soft delivery switch distinct from the feature flag.

## Code review

**The view is gone from the binary.** v1's native diff reviewer
(`ui/code_review.rs` + `app/code_review.rs`) went with `src/ui`
(`openspec/changes/v2-code-review/` has the design that was written for a native
replacement, `v2-parity-gaps` tracks it); v1 keeps it on the `v1.x` branch. It came
back **as a pane**, in its own repository:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) —
installed by clone (`thurbox-cli plugin install
git+https://github.com/Thurbeen/thurbox-code-review`), takes the `center` switch
slot beside the agent, reclaims `Ctrl+X`/`F7`, and is the first consumer of
`thurbox.diffs` anywhere. It is not vendored here and not bundled: a change to the
snapshot's diff shape or to `command("focus", { toggle })` breaks it, so treat it as
a downstream consumer of that contract.

What survived, because it is not view code:

- **`session::review`** — the pure diff types and `parse_unified_diff`, which is
  why they live in `session` rather than beside a renderer.
- **`storage::review`** — `review_comments` + `review_marks` (schema v38), keyed on
  the write-once `sessions.base_branch`. Comments already written are still there.
- **`git::diff_against{,_on}`** (a base branch) / **`git::working_diff_on`** (the
  uncommitted changes) and **`kernel::diff`** — diffs are produced on a worker and
  published into the snapshot, bounded at `MAX_DIFF_BYTES`. The working-tree one
  folds in **untracked files** (`git diff --no-index -- /dev/null <path>`, capped at
  `git::UNTRACKED_FILE_CAP`, overflow reported as `untracked_omitted`): `git diff
  HEAD` cannot show a file git has never been told about, which made the default
  target report "no changes" after an agent wrote new ones. There is deliberately no
  body-only `git diff HEAD` helper — having one is how that omission happened.
  Rationale + the rejected temporary-index approach: ADR-P6's diff bullets in
  `docs/PERFORMANCE.md`.

So a review plugin has its data layer waiting for it. Two rules from the v1 design
still apply if you build one: **1 logical diff row = 1 selectable unit** (wrapping
expands only *visual* rows; selection and comment anchoring stay logical), and the
diff types stay in `session` (architecture rule).

## Demo Video

> **The VHS recordings are stale.** Every clip a tape produces was recorded against
> v1 and shows panes the interface no longer has — code review, the file viewer, the
> tasks and automations panels. The tapes themselves drive v1 chords, so they need
> rewriting before re-recording is worth doing. Until then the website and README
> advertise an interface that is gone. This is the most visible inaccuracy left.
> `doom-easter-egg.mp4` is the exception: it is not a tape, and it was re-recorded
> against v2 when Doom became a plugin (below).

The media is **generated**, not hand-recorded. One script drives the *real* TUI via
[VHS](https://github.com/charmbracelet/vhs) (needs `vhs` + `ffmpeg` + `ttyd` +
`tmux`) and writes GIF **and** MP4 into `docs/media/`:

```bash
scripts/demo/record.sh                 # regenerate ALL demo videos
scripts/demo/record.sh theme search    # re-record a subset
```

One VHS tape each (`scripts/demo/<feature>.tape`); no args = all, otherwise tape
stems. Every clip uses **real agent CLIs**, one session per installed CLI, with
`HOME` overridden so agents boot with fresh history (keyring-authenticated CLIs stay
logged in but show no account email).

**The scenario is a realistic one, not a UI tour.** The repo is a vendored snapshot
of thurbox's own tree (a fixed file list copied into the throwaway `HOME` and `git
init`ed there — MIT, already local, so recordings stay hermetic and offline).
Sessions are named after the *work*, not the agent (`fix-osc52-tmux`,
`add-wsl-host-tests`, `perf-session-order-cache`, `docs-remote-hooks`), so the list
reads as one backlog with four branches in flight. The seeded tasks/automation and
the queries typed in the tapes are keyed to that same narrative, so **editing one
means editing the others**.

It runs fully isolated: a dev build uses the `thurbox-dev` socket and XDG subdirs,
and the script points `TMUX_TMPDIR` + `XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a
throwaway temp dir. **`TMUX_TMPDIR` is essential** — the `thurbox-dev` socket *name*
is shared by every dev build, so without a private socket directory the cleanup
`kill-server` would tear down dev sessions you have running.

`.github/workflows/pages.yml` copies the mp4s into `website/assets/` at deploy time
and `README.md` embeds the gifs, so regenerating them propagates everywhere.

**The website's `iddqd` easter egg is a separate recording.**
`scripts/demo/record-doom.sh` writes `docs/media/doom-easter-egg.mp4` plus its
poster, and it is not a VHS tape: asciinema records the real TUI and agg rasterises
the cast. Doom is a **plugin** there, not an agent — the script installs
[`thurbox-doom`](https://github.com/Thurbeen/thurbox-doom) into the throwaway
interface directory, seeds the `program` grant the settings modal would have written
(trust is a decision made *in* a running interface, so there is deliberately no CLI
for it), and presses `f7`. Everything the clip keeps is timed from that press, so
read the phase comments before changing any of the numbers — including which frame
becomes the poster, since Doom flashes the whole view red when the player is hit.

## Architecture (plugin kernel)

The interface is **Lua running on a Rust kernel**. `thurbox` boots the kernel,
which reads `ui/` and renders whatever plugins it finds; there is no built-in
pane. v1's `src/app` (TEA model/update/view) and `src/ui` (35 render modules) were
deleted when the kernel took the binary name — v1 lives on the `v1.x` branch.

### The five rules

1. **Four node kinds, forever** — `text`, `box`, `input`, `surface`. Everything
   else composes in `ui/lib/widgets.lua`. `tests/kernel_mvp.rs` asserts the count.
2. **Layout resolves before render** — rects are computed first, then each plugin
   is called with its own. Plugins declare size *statically*, in their declaration
   table, which is what breaks the circularity.
3. **Snapshot-read, command-write** — reads come from an in-memory snapshot and
   return instantly; writes are commands accepted now and surfaced later. Lua never
   blocks, so no plugin can stall the loop on SQLite, git or an unreachable host.
4. **Capabilities by absence** — an ungranted capability is *not in the
   environment*. `io`, `os`, `debug`, `package` and the loaders are withheld, and
   `thurbox.yml` makes selene enforce that statically. One capability cannot be
   expressed that way: a program pane is asked for through `command`, which every
   plugin has, so `thurbox.granted` reports the grant instead — a boolean about a
   decision already made, which withholds nothing.
5. **Anything touching the world runs on a worker** — terminal attach, commands,
   diffs, metrics, git stats, repository reads, update checks, and programs a
   plugin asked for.

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no crate-internal references
agent    ← session (+ paths/shell utils; NEVER git)
kernel   ← session + storage + sync + paths + session_ops + git
           (+ agent/usage by fully-qualified path only)
main     ← the coordinator: the loop, the workers, the chrome
```

Enforcement is an **allowlist**: every module under `src/` needs a `ModuleRules`
entry naming what it may reference in *any* form (`use`, `pub use`, brace groups,
fully-qualified `crate::…`), so a new module fails the test until its place is
declared. `main` is `EXEMPT`, as `app` was before v1 was retired. `kernel` reaches
`agent`/`usage` by fully-qualified path only — never `use` — so every crossing into
the side-effect layer is visible at its call site, the rule `session_ops` and `cli`
already follow.

### Module Responsibilities

- **`kernel/`** — the interface. `node` (four primitives), `layout` (rects before
  render), `convert` (Lua table ↔ node), `paint` (node → ratatui), `host` (the VM,
  reload, isolation, capability grants), `registry` (keys + settings plugins
  declare), `modals/` (help, settings, theme picker — chrome about thurbox itself,
  which plugins contribute *data* to rather than replace), `bands` (the top/bottom
  bars), `snapshot` (the read side), `command` (the write side), `terminal/` (live
  PTY surfaces: the attach machinery, plugin program panes, link detection +
  OSC 8 painting), `selection` (mouse text selection over a pane),
  `consent` (the one-time v1→v2 gate), plus the worker-backed
  stores: `diff`, `metrics`, `repos`, `runs`, `updates`, `files`, `notify`,
  `theme`, `perf`, `bundled`, `inventory`, `packages` (the spec, the lock, and the
  install/converge/withdraw operations — sharing `bundled`'s decision matrix).
- **`agent/`** — side-effect layer, unchanged by the retirement. `AgentProvider`
  - `GenericProvider` build the CLI invocation from a declarative `AgentDef`;
  `Session` wraps a `SessionBackend`; `TmuxBackend` runs tmux over a
  `TmuxTransport` (`Local` / `Ssh` / `Wsl`). Output is read into
  `Arc<Mutex<vt100::Parser>>`, input written over an mpsc channel.
- **`session/`** — plain data: `SessionId`, `SessionStatus`, `SessionInfo`,
  `SessionConfig`, `AgentDef`/`AgentRegistry`, `HostDef`/`HostRegistry`,
  `PluginSpec`/`PluginLock`/`PackageManifest` (`plugin_spec`, so the kernel and the
  CLI share one definition), plus the logic the kernel needs and cannot import
  `agent` for (`hyperlink`, `review`, `theme_config`).
- **`git/`** — every `git` invocation, split by concern: `command` (the one
  place a `git` process is built, and where the inherited `GIT_*` scrub lives),
  `plugin` (clone/checkout of a plugin working copy), `remote` (running a command
  over ssh/`wsl.exe`, the PowerShell encoding, and decoding what came back —
  ADR-13's psmux divergences), `discovery` (listings, path classification,
  child-repo scans, each a POSIX/PowerShell script pair over one line protocol),
  `diff` (diffs, branches, commits, worktree stats) and `worktree` (create, sync,
  remove — and the stale-lock/stash/transient-error retries). `git::*` is still
  one flat surface; no caller names a submodule.
- **`coordinator/`** — `main`'s own body. `App` and its state stay in
  `main.rs`; its behaviour is here, grouped by what it is for: the loop and its
  workers (`mod`), then `commands`, `publish`, `draw`, `input`, `mouse`, `focus`
  and `interface`. Still one model and one loop — splitting `App` itself is what
  ADR-22 rejected, and the invariants that matter hold *across* these groups.
- **`ui/`** (Lua, not Rust) — `layout.lua` is the arrangement; `lib/` holds
  widgets, theme roles, fuzzy match, text input, trees; `plugins/` holds the panes.
- **`cli/`** — `thurbox-cli` subcommands, including `plugin dir|new|check|list|install|sync|update|remove`
  for writing an interface with no TTY.

### Event Loop (`src/main.rs` + `src/coordinator/`)

```text
tokio::main → load config + settings → heal extensions → arm the heartbeat
  → the v1→v2 consent gate (kernel::consent, before the terminal is taken)
  → resolve ui/ → build the Lua host → open SQLite → init terminal → loop {
    resolve layout → call each plugin with its rect → paint
    → poll workers (terminals, commands, diffs, metrics, repos, runs, updates)
    → drain Lua's command queue → dispatch keys through the registry
} → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log` (stdout is the TUI's)
- A panic hook restores the terminal, pops the kitty flags and disables mouse
  reporting before printing — otherwise the shell inherits a raw-mode terminal
  streaming mouse reports

## Pre-commit Hooks

19 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats, shellcheck, rumdl, selene, stylua, prettier,
  htmlhint, stylelint, eslint
- **pre-push**: commit history check (`cog check`)

Shell scripts are linted with **shellcheck** (config in
`.shellcheckrc`); install it from your package manager (it is not a
cargo crate — `scripts/install-dev-tools.sh` prints a reminder).

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `TmuxBackend` over a `TmuxTransport`
  (local `tmux -L thurbox`, or `ssh <dest> tmux …` for
  `ssh:<host>` backends from `hosts.toml`)
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Session state in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected);
  agent definitions in `~/.config/thurbox/agents.toml`;
  remote SSH hosts in `~/.config/thurbox/hosts.toml`
- Requires tmux >= 3.2

## Keybindings

Every chord goes through **one registry** (`kernel::registry`). Plugins *declare*
keys in their declaration table; the kernel resolves a press to an action and hands
it back to the plugin that claimed it. There is no hardcoded table to keep in step
with a help screen, because the help modal renders the registry.

Global chords (kernel-owned):

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit (detach sessions) |
| `Ctrl+N` | New session (the creation flow) |
| `Ctrl+H` / `Ctrl+L` | Focus previous / next pane |
| `Ctrl+J` / `Ctrl+K` | Select next / previous session |
| `Ctrl+,` / `F6` | Settings (`]` for the Interface tab) |
| `Ctrl+Y` / `F4` | Theme picker |
| `F1` / `Ctrl+G` | Keybindings help |
| `F10` | Reload the interface from disk |
| `F12` | Perf HUD |

Everything else belongs to a plugin and is listed in `F1`. Rebindings persist to
`ui.json` beside trust and the disabled set — a *user decision*, distinct from the
delivery facts in `.bundled.json`.

Two properties the registry holds and `tests/v2_keymap.rs` asserts:

- **A plugin-scoped claim does not outrank a global one.** This is why search does
  not take `Ctrl+P`/`Ctrl+N`: doing so would take `Ctrl+N` from new-session
  everywhere.
- **A chord freed by a removed pane stays unbound** rather than being silently
  reused by whatever loads next.

**Terminal passthrough.** thurbox's chords share the `Ctrl+<letter>` namespace with
readline (`Ctrl+A`, `Ctrl+E`, `Ctrl+W`, `Ctrl+U`, `Ctrl+R`, `Ctrl+D`, …). While a
session terminal is focused, a chord a plugin flags as passthrough reaches the agent
instead. Navigation and app-control chords (`Ctrl+H/J/K/L`, `Ctrl+Q`, `Ctrl+N`) are
**never** deferred — they are the way out of a focused terminal.

**macOS.** The kitty keyboard protocol is pushed at startup
(`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`, gated on
`supports_keyboard_enhancement()`, popped in `restore_terminal` and the panic hook
because `ratatui::restore()` does not). That is what makes `cmd+…` bindable at all
(iTerm2 3.5+, kitty, WezTerm, Ghostty — not Terminal.app) and what separates
`Ctrl+/` from the bytes a legacy terminal sends for it. Emulator-level shortcuts
(`Cmd+Q/W/N/T/C/V`, `Cmd+K`, …) never reach the TUI; only bind what the terminal
leaves free. F-keys need `Fn` on Mac laptops unless "Use F1, F2, etc. as standard
function keys" is on.

## Themes

Thirty-six palettes — twenty-eight dark, eight light; the enumeration is in
`session::theme_config` and `docs/FEATURES.md`. Users add their own in
`~/.config/thurbox/themes.toml` (a built-in `base` plus per-colour overrides); they
appear in the picker after the built-ins and persist by name exactly like a preset.

`kernel::theme::Themes` resolves them and publishes **roles** to Lua
(`ui/lib/theme.lua`), so a plugin asks for `theme.accent` or `theme.muted` rather
than a colour — which is what lets one plugin look right under all thirty-six. Pick
one with `Ctrl+Y` (or `F4`, avoiding terminals that take Ctrl+Y as DSUSP); the choice
persists in SQLite under `metadata.active_theme`, and other thurbox processes pick it
up within a tick via `PRAGMA data_version`.

The picker (`kernel::modals::theme`) filters behind `/`, mirroring the file-viewer
and review find so its keys stay consistent: `j`/`k` (+ arrows, `PageUp`/`PageDown`,
`g`/`G`, `Home`/`End`) navigate, and only after `/` do letters append to a query —
matched against display name *and* stable id with a live `matched/total` count.
Entries group under `Dark`/`Light` headers drawn *inside* their entry's row, so
selection, hitboxes and the scrollbar stay in entry space and a header disappears
with its filtered-out section. The index addresses the **match** list, so refining a
query keeps the cursor on the same theme when it survives — narrowing cannot apply a
palette other than the previewed one.

The v1→v2 consent gate paints itself from the user's active palette for the same
reason (`kernel::consent::Skin`): a gate in somebody else's colours reads like a
different program.

## Settings panel

`Ctrl+,` (or `F6`) opens a **kernel-owned modal** (`kernel::modals::settings`) —
chrome about thurbox itself, so it overlays the arrangement, captures input and
stays out of the focus ring. Plugins contribute *data* to it: declare
`{ id, desc, default }` and the modal grows a row.

Two halves on one screen:

- **Plugin settings** go through `Registry::set_setting` — in-process, effective on
  the next frame. Nothing to save, no Cancel.
- **Core settings** are `settings.toml`, written back through a `toml_edit`
  `DocumentMut` so the seed's documentation comments survive.

Whether a core row applies live or waits for a restart is **asked of
`Settings::restart_only_differs`**, the same function `Config::adopt` consults —
never a second list beside the field. A hand-written copy had already drifted from
it, promising both panel-width scalars applied live while `adopt` froze them and
reported `NeedsRestart`. Restart-only rows are marked `⟳`.

The panel is handed **`Config::on_disk`**, not `Config::in_force`. They are
different documents on purpose: a restart-only change is written to the file and
deliberately *not* taken into force, so a panel drafting from what is in force
proposes reverting every such change already saved — one visit saved
`features.mouse = false`, and the next save of anything at all put it back. Two
thirds of the core rows are restart-only, which made it read as "my settings do
not survive a restart".

`]` switches to the **Interface tab** (`kernel::modals::interface`): every file, where
it came from (bundled / edited / yours / installed / removed), whether it is on
screen, and `r` restore · `d` delete · `space` turn off · `t` trust. It was a pane
once — an honest test of whether the plugin API could build a pane that lists panes
— and is chrome now because a recovery tool must not be the thing that is broken.

It is therefore **the recovery path for a broken interface**, and the shape of that
is not symmetric: a `failed` row sorts to the top with its load error in the footer,
but `r` only writes back a copy thurbox *ships* — it refuses for a file the user
wrote ("thurbox ships no version of it") and points an installed pane at
`thurbox-cli plugin sync`. For a pane of the user's own the way back is `space`:
present on disk, not loaded, so nothing tried to load it and its error is silent
until it is switched on. Each of the four keys reloads the interface, so the result
is on the next frame. Documented for users in `docs/PLUGINS.md` → **When something
goes wrong**; the same three answers with no TTY are `plugin list` / `plugin dir` /
`plugin check`.

`settings.toml` is **live-reloaded** (mtime poll): an outside edit re-applies the
live half and toasts, noting a restart when `restart_only_differs` says so.

> `[features] code_review`, `file_viewer`, `tasks`, `info_panel` and
> `global_search` gated surfaces the interface no longer draws, and
> `three_panel_min_cols` sized a column that no longer exists. None of the six is
> read by anything: they are parsed so an existing `settings.toml` keeps loading
> rather than failing on an unknown key, and setting one does nothing in either
> direction. The settings panel does not offer them.

## OpenSpec (spec-driven changes)

Non-trivial changes can be planned through
[OpenSpec](https://github.com/Fission-AI/OpenSpec) before any code is written.
It is **tooling, not a gate** — small fixes still go straight to a commit.

Six skills drive the loop: **propose** (draft `proposal.md` → `specs/` deltas →
`design.md` → `tasks.md`) → **apply** (implement the tasks) → **archive** (fold
the deltas into `openspec/specs/`, move the change to
`openspec/changes/archive/<date>-<name>/`). `explore` (think first), `update`
(revise artifacts), and `sync` (merge deltas without archiving) fill the gaps.

They are installed for the three agents this repo already configures, each in
that agent's own layout (the skills are identical; only the command alias and
its spelling differ):

| Agent | Skills | Commands | Invoked as |
|-------|--------|----------|------------|
| claude | `.claude/skills/openspec-*/` | `.claude/commands/opsx/` | `/opsx:propose` |
| pi | `.pi/skills/openspec-*/` | `.pi/prompts/opsx-*.md` | `/opsx-propose` |
| opencode | `.opencode/skills/openspec-*/` | `.opencode/commands/opsx-*.md` | `/opsx-propose` |

- **Layout**: `openspec/config.yaml` (schema `spec-driven`, plus optional
  project `context` and per-artifact `rules`), `openspec/specs/` (current
  requirements), `openspec/changes/<name>/` (in-flight work). One shared
  `openspec/` tree serves all three agents.
- **CLI**: `npm install -g @fission-ai/openspec`. The skills are restricted to
  `Bash(openspec:*)`, so the binary must be on PATH — an `npx` fallback will
  not work. `openspec update` refreshes the skills after a CLI upgrade;
  regenerate them with `openspec init --tools claude,pi,opencode` (re-running
  `init` is additive — it leaves already-configured agents intact).
- Skill and command files are generated — edit `openspec/config.yaml` to shape
  the workflow rather than hand-patching them, since `update` overwrites them.
- `.opencode` joins `.claude`/`.pi`/`openspec` in `rumdl`'s exclude list
  (`.rumdl.toml`). The generated skills and change artifacts would otherwise
  block commits on prose we don't author; excluding the whole agent dir (rather
  than the generated files inside it) is how `.claude`/`.pi` were already
  treated.

## Writing an interface plugin

The bundled set is deliberately small: `10_sessions`, `20_agent`, `65_search`, plus
three floats that occupy no slot — the creation flow (`70_new_session`), the
confirmation (`60_confirm`) and the restore list (`80_restore`, v1's `Ctrl+U`).
What v1 had and this does not is listed in `openspec/changes/v2-parity-gaps/`.

```bash
thurbox-cli plugin dir            # which directory is live, and which rule chose it
thurbox-cli plugin new notes      # a starter that already loads
thurbox-cli plugin check          # load it the way thurbox does; non-zero on failure
thurbox-cli plugin list           # the inventory the Interface tab shows
thurbox-cli plugin install top    # a distributed pane, recorded in plugins.toml
thurbox-cli plugin sync           # make the directory match the spec
```

Two of v1's surfaces are maintained as **out-of-tree panes**, each its own
repository, installed by clone:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) (the diff
reviewer, asks for `run` only for the targets the kernel does not compute) and
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) (v1's info
panel, no capabilities — everything from the snapshot). Neither is bundled or
vendored; both are downstream consumers of the published `thurbox.*` shape.

Two **example** panes live in `ui-plugins/` (`tasks`, `top`) and install by bare
name; `docs/examples/{plugin,composite,layout}.lua` are copied by hand. They are
examples to read and copy from, not a catalogue thurbox maintains for anyone —
`EXAMPLE_PLUGINS` is the list a bare name and a typo suggestion resolve against.
`check` fails on a pane that **loaded but which no arrangement places** — the only
failure with no symptom — and prints the `layout.lua` line to add.

The directory ships its own guidance for whoever edits it: `README.md` is the
reference, and **`AGENTS.md`** is the operational half a coding CLI loads as context
without being asked — which is what stops "install this plugin" being read as a
package-manager request rather than `thurbox-cli plugin install`. `CLAUDE.md` and
`GEMINI.md` beside it are one-line pointers, not copies, so there is one file to
keep true; the `flow` extension surfaces its own spec the same way, as symlinks it
can make because it copies its own files.

Two rules pick the directory: `THURBOX_UI_DIR` if set, otherwise the user's copy
(`~/.config/thurbox/ui/`, materialised from the embedded interface on first run,
preserving edits). A third rule — a `./ui` beside the working directory, winning
automatically — was removed: it made the interface the one config that ignored the
dev/release split, so `cargo run` in the repository read `~/.config/thurbox-dev` for
agents, settings, themes and the database and the *checkout* for its panes, silently.
Editing a checkout's interface is now an explicit `THURBOX_UI_DIR` (`just tui-ui`),
and startup reports the directory whenever there is a question about it — an override
is in force, the fallback was used, or this is a dev build. The user-copy path is
derived
from `paths::config_file()` like every other config path — `agents.toml`,
`hosts.toml`, `settings.toml`, `themes.toml`, `extensions/`, `ui.json` — so a **dev
build reads `~/.config/thurbox-dev/ui`** and cannot touch the release copy.
`THURBOX_CONFIG_DIR` overrides that anchor and thurbox injects it into every
session it spawns, which is why a `thurbox-cli` run *inside* a session resolves
against that session's config dir rather than the dev default. `config show` prints
the resolved `ui_dir`/`ui_json` beside the rest. The resolved directory is
made **absolute** — trust, the disabled set and rebindings are keyed by
`ui_dir.join(file)` and compared verbatim, so a relative `ui` would be shared by
every checkout on the machine. It is *not* canonicalised: that would return a
`\\?\D:\…` extended-length path on Windows and resolve `/var` to `/private/var` on
macOS, and this path is shown to people.

**Delivery vs. decision vs. composition.** `.bundled.json` records what delivery did
(bundled / edited / yours / removed / installed); `ui.json` records what the *user*
decided (disabled, trust, rebindings); `plugins.toml` records what the interface is
**composed of** and `plugins.lock` what each entry resolved to.
Deleting a bundled file is how you remove it — delivery records
the removal and never writes it back, which is what makes a differently-named
replacement possible on equal terms. Turning one **off** is a third thing: present on
disk, intact, not loaded — implemented by `build` not reading the file, so a disabled
plugin declares no keys, occupies no slot and is granted no capability. A broken one
can be switched off to get a working interface back.

**A plugin that carries more than Lua is a repository** (`ExtensionSource::Git`,
`git::clone_plugin`). The fetch path returns a `String` and decodes remote output
lossily, so a binary through it is *corrupted rather than refused* — which is why
payload arrives only by clone and that path stays Lua-only. Recognition is
**explicit**: `git+<url>`, a `.git` suffix, or `git@host:path`; a bare `https://` URL
keeps meaning "fetch the manifest's files from this base". The working copy lands at
`<ui_dir>/<name>/` and **keeps its `.git`**, so `update` is a fetch and git owns "your
edits are yours" (a dirty tree is never moved and reports `kept`). The lock records
the **commit**, not the ref, which is what makes a spec reproducible and why there is
no checksum field. Two consequences elsewhere: `build` loads panes the spec names
outside `plugins/` (`is_nested_pane`; the load order still comes from the basename's
numeric prefix) and `sources()` inventories those panes but deliberately does **not**
walk the rest of a working copy. `.git` is watcher noise (`watch::is_noise`) — a fix
in its own right, since it already affected anyone versioning their own panes, and the
symptom is not "reloads too often" but "stops reloading while git is busy".
`thurbox.platform` is published so a plugin shipping several binaries picks one
itself; platform selection is deliberately not a manifest field.

**Panes are installable** (`kernel::packages`, `session::plugin_spec`). `plugins.toml`
in the interface directory lists a `src` (a bare name resolving to `ui-plugins/<name>`
at the binary's release tag, a URL, or a path — `extension_config::resolve_source_in`,
one resolver for both kinds of thing), a destination `file`, and an optional `pin`;
`plugins.lock` records what each resolved to plus the digest of every file delivered.
`plugin sync` converges the directory to the spec, which reduces an agent's job to
*edit one file, run one command, read the exit status*. Delivery semantics are
**shared with the bundled path, not reimplemented**: both call `bundled::decide`, so an
edit is preserved and a deletion is remembered for a package exactly as for a shipped
file. Two consequences worth knowing: the manager never writes `layout.lua` (placement
stays hand-authored, and `check` fails loudly on an unplaced slot instead), and a
package may deliver shared modules only under `lib/<its own name>/` — enforced at
install time, since `require` splits on every dot and would otherwise let a package
replace `lib/theme.lua`.

**Trust for an installed pane is keyed to `(src, pin)` *and* the lock's digest**
(`registry::Granted::Managed`, `inventory::Trust::resolve_installed`). Contents alone
would report every ordinary release as tampering; `(src, pin)` alone would let an
upstream re-tag of the same version inherit a grant made to what that version used to
be. Both are checked, and `ui.json`'s bare-string grant form is still read so an
upgrade forgets nothing.

**A plugin can run a program you interact with** (`Capability::Program`,
`kernel::terminal`'s `programs` map). `run` captures output once with no stdin and
no tty, so it cannot give you `htop`, `lazygit` or a REPL; a *program pane* holds a
real terminal — keystrokes go to it, it is resized to its rect, it survives an
`F10` reload. The pane belongs to the **plugin**, not a session: the plugin writes
`command("program", { text = "watch", repo = "htop" })` and draws
`{ type = "surface", program = "watch" }`, and the kernel stamps the owner from the
plugin being rendered, so naming another plugin's pane is impossible by
construction. Reuses the companion-shell machinery whole (`ProgramPane` mirrors
`ShellPane` over the same `Session::wire_up`); the differences are a third window
prefix (`tbp-`, invisible to `discover`'s `tb-` filter — hence `find_window`), and
that **nothing is persisted**: the window name is deterministic, so re-adoption
after a restart is a lookup and there is no stored id to go stale. Gated by its own
capability, **not** `run`'s — `run` is bounded (256 KB, 600 s, 4 at a time) and an
interactive program is none of those, so an existing grant must not silently widen.
Four panes per plugin. `thurbox.granted.<name>` is how a pane knows, since
`command` is present whether or not it may.

**A plugin can run a program** (`kernel::runs`) — `git status`, `docker compose ps` —
in the session's working directory, and on that session's own host for a remote
session. `run(key, program, opts)`; the answer arrives next frame as
`thurbox.runs[key]`, so Lua still never blocks, and asking **every frame is the
intended pattern** because a fresh answer is a map lookup rather than a process
(`request` refuses a duplicate while the answer is fresh *or* while a run for that
key is in flight). Bounds are the kernel's: output capped with truncation flagged, a
timeout, four at a time with the rest queued.

This is the first capability that reaches outside thurbox, so it is granted **per
plugin**: declare `capabilities = { "run" }` and get nothing until the user trusts
the file (settings → Interface → `t`). Trust is keyed by absolute path with the
digest recorded, so a changed trusted file reads `trusted · modified`. It is
deliberately **not a sandbox** — a program thurbox spawns has the user's authority —
and the position is that thurbox can only refuse to run things unasked.
`docs/examples/composite.lua` is the worked example.

The implementation lives per-call: `LuaHost::enter` stamps the current plugin and, in
the same breath, binds `run` to the implementation or to nil, and `enter_nothing` is
the other half for Lua that belongs to no plugin (`layout.lua` declares no
capabilities). The implementation itself is held in the VM's **registry**, never its
globals, because a plugin chunk's `_ENV` *is* the globals table — it sat there as
`__run_impl` once, which handed every untrusted plugin the capability under a second
name.

- `docs/V2-KERNEL.md` — the kernel's shape, its five rules, and the traps
- `docs/PLUGINS.md` — writing a plugin; **Start here** needs no TTY, and **Traps**
  lists the mistakes that are invisible until runtime
- `openspec/changes/v2-*` — the changes, their specs, and what each got wrong

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices
- `docs/CONFIG.md` — Every config file/env var/DB setting in one place
- `docs/AGENTS.md` — Each built-in agent's exact config + behavior, and
  the checklist for adding a new built-in
- `docs/PERFORMANCE.md` — Render/tick performance: demand-driven redraw,
  perf counters, the session-order cache, and how to measure

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
