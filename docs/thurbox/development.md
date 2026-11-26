# Development

How to set up a reproducible thurbox dev environment and run the app in an
isolated sandbox.

The repository is one Cargo workspace: thurbox is the member at
`crates/thurbox`, and the trees that are not a crate (`ui/`, `examples/`,
`vendor/`) stay at the root. Cargo commands run from the root and
cover every member; a source path in these pages (`src/…`, `tests/…`) is the
thurbox crate's.

## 1. Toolchain — the dev environment

### Recommended: Nix flake

The `flake.nix` provides the tools CI uses — the Rust toolchain (read from
`rust-toolchain.toml`), interactive Bash, `tmux`, `shellcheck`, `bats`, Node,
`cargo-nextest`, `cargo-deny`, `cocogitto`, `just`, the three Lua gates
(`selene`, `stylua`, `lua-language-server`), `jq`, and the demo stack
(`vhs`/`ffmpeg`/`ttyd`). Interactive Bash is explicit because the real-pane
tests exercise Readline's bracketed-paste mode; Nix's stdenv Bash omits those
interactive builtins.
`flake.lock` pins nixpkgs, so the shell's tools move only when someone runs
`nix flake update`; the Rust toolchain still follows `stable` from
`rust-toolchain.toml`. The same flake also packages thurbox itself
(`nix/package.nix`), which CI's `nix` job builds; the rest of CI installs its
tools without Nix, so the shell is a local convenience, not what CI runs.

The package's version must not contain `-dev`: `build.rs` would then set the
`dev_build` cfg, which moves the build onto the `thurbox-dev` socket and data
directory. A `0.0.0` version still reads as unreleased at runtime, so
`thurbox-cli update` never tries to replace a binary in the read-only store.
The package skips its tests, which CI's `Nextest` job runs. It appends `tmux`
and `git` to `PATH`, so a `tmux` or `git` the user has keeps winning.

```bash
# one-time, if not done already: enable flakes
#   mkdir -p ~/.config/nix && echo 'experimental-features = nix-command flakes' >> ~/.config/nix/nix.conf

nix develop           # enter the pinned shell
# ...or, with direnv installed, once:
direnv allow          # auto-enters the shell on `cd` (see .envrc)
```

Two tools aren't packaged in nixpkgs yet (`prek`, `rumdl`); the shell prints a
hint to install them via `scripts/install-dev-tools.sh`. That script installs
rumdl at the version CI pins, which `just lint` and the commit hook enforce
(`scripts/ci/check-rumdl-version.sh`).

`just clippy-windows` (part of `just lint` and the pre-push hook) also needs
`rustup target add x86_64-pc-windows-gnu` and a MinGW gcc
(`x86_64-w64-mingw32-gcc`); the install script adds the target and names the
package for your OS. Without the target (the Nix shell ships none) the recipe
prints `SKIPPED` and leaves the check to CI's `Windows (clippy)` job.

### Fallback: no Nix

```bash
scripts/install-dev-tools.sh   # cargo-binstall/cargo install the dev tools
prek install                   # install the git hooks
```

You'll also need, from your package manager: `tmux >= 3.2`, `shellcheck`,
`bats`, Node + npm (website linters), `git`, and the three Lua gates `just lint`
runs — `selene`, `stylua` and `lua-language-server`. Run `npm ci` once, or
`just fmt`'s website half exits 127 on a fresh checkout.

For opt-in RMUX sessions, install RMUX >= 0.10.0 separately; the dev flake
provides tmux. [config.md](config.md#multiplexer-requirements-and-rmux-setup)
records version evidence and RMUX platform limits.

## 2. Everyday tasks — `just`

`just` (in the dev shell) is the task entrypoint — run `just` for the list:

| Task | What it does |
|------|--------------|
| `just build` | build the dev binaries (`thurbox` + `thurbox-cli`) |
| `just test` | `cargo nextest run --all`; the tests that drive an installed agent CLI run only with `FLEET_REAL_AGENTS=1` |
| `just test-one NAME` | one test by name (`cargo nextest run -E 'test(NAME)'`) |
| `just test-scripts` | the bats suites CI runs: `scripts/install.bats`, the pull-request-title checker, the release-line gate, `packaging/winget/winget.bats`, `scripts/dev/e2e/windows-vm.bats`, `scripts/dev/lib/sandbox-env.bats` and the rumdl version check (needs `bats`) |
| `just lint` | fmt-check + clippy + cargo-deny + Windows clippy + rumdl (CI's version) + shellcheck + selene, stylua and lua-language-server |
| `just clippy-windows` | the `Windows (clippy)` CI job, cross-checked from Linux (`--target x86_64-pc-windows-gnu`); needs that target's standard library and a MinGW gcc, and skips without the target (the Nix shell ships none) |
| `just fmt` / `just fmt-lua` | format Rust + website / format the Lua interface |
| `just lua-api` | regenerate `thurbox.d.lua`, `thurbox.yml` and the selene probe from `src/kernel/lua_api.rs` |
| `just arch` | architecture-rule + rustdoc checks |
| `just hooks-install` | `prek install` |
| `just smoke` | black-box TUI test: the real binary on a pty (`tests/tui_e2e.rs`) |
| `just reap-tmux` | reap orphaned *test* tmux servers a signal left behind (below) |
| `just bench` | what a frame costs, piece by piece (`benches/frame_cost.rs`) |
| `just perf` | what the whole binary costs under load (`scripts/dev/perf-run.sh`) |
| `just lab HOST` | drive the e2e tests against a real SSH host (`scripts/dev/e2e/real-host.sh`) |
| `just tui-ui` | run the dev TUI against this checkout's `ui/` (`THURBOX_UI_DIR`) |
| `just sandbox*` | dev runtime sandbox (below) |

### `just reap-tmux` — the servers a guard could not reap

Every harness that starts a tmux server holds a `TmuxServer`
(`crates/runtime-mux/tests/support/tmux_server.rs`), whose `Drop` kills it. That covers every way
a test ends in-process, a panic included, which is what teardown spelled as a
`cleanup()` call at each exit point did not: the socket file lives in a
directory the run owns, so a test that skipped its teardown left a server with
no socket, unreachable by the very command that would have killed it. One
machine reached 400 of those, with 4 sockets between 433 servers.

A destructor cannot run for a signal, though — a nextest `slow-timeout`
termination, `kill -9`, an OOM kill — so that one case still leaks, and
`just reap-tmux` is the sweep for it. It kills only processes that are both a
tmux **server** of yours on one of the suite's own socket names and unreachable
(no socket file), which is why it can never touch `thurbox`, `thurbox-dev` or
anything you are attached to. `just reap-tmux --dry-run` lists without killing.
Linux only: the socket directory is read from `/proc/<pid>/environ`, and there
is no portable equivalent.

`just bench` and `just perf` are the two measuring instruments, and they answer
different questions: the bench times the *pieces* of a frame against the real
`ui/`, while `just perf` runs the whole binary against real tmux panes and
reports CPU. Neither is in CI — wall-clock timing stays out of the gate
(ADR-P5) — and a claim from either is a paired before/after at a stated
terminal size and session count, never a single absolute number. Both are
explained in [`docs/thurbox/performance.md`](performance.md).

### PTY test negotiation

`tests/tui_e2e.rs` emulates both directions of terminal capability detection.
The default legacy terminal answers the primary device-attributes query, while
ignoring the kitty keyboard query. Crossterm can then report unsupported keyboard
enhancements without waiting for its negotiation timeout. Kitty scenarios still
answer both queries and track pushed flags per screen. An explicit silent-terminal
scenario retains the timeout fallback and press-only input assertions.

The regression checks the actual reply bytes written to the PTY, the unsupported
release state, and repeated NUL key presses. Fragmented queries are checked for
both terminal types, including that each query is answered once. No timing
threshold is used to assert this optimization. The nextest PTY group still runs
one test at a time alongside the other test binaries.

Measured on x86-64 Linux with Rust 1.99.1, cargo-nextest 0.9.137, the locked
dependencies at `206ae013`, default features and the unchanged Cargo test profile
(`opt-level = 1`, dependency optimization 3, line tables). Both runs used
`CARGO_BUILD_JOBS=2 cargo nextest run --test tui_e2e`, the same dedicated disk target,
the default nextest profile, no retries and the existing single-thread PTY group.
Compilation finished before each timed run. These paired snapshots preceded the
separate deterministic refusal-fixture correction from PR #1539; they do not
measure that integrated fixture's runtime. The before tree included the same two
new test cases and reply instrumentation; its new legacy assertion failed on an
empty reply. The after tree passed all 163 runnable cases; the same pre-existing
ignored case stayed ignored. Test names, ignore flags and filter matches were
identical, and the responder optimization preserves the original assertions.

| Warm command measurement | Before | After |
|--------------------------|--------|-------|
| Wall time, including warm Cargo startup | 626.991 s | 251.048 s |
| Child user CPU | 22.703 s | 21.147 s |
| Child system CPU | 15.456 s | 13.447 s |
| Largest child peak RSS | 93,556 KiB | 93,412 KiB |
| Reported block input/output operations | 249,992 / 504 | 3,032 / 280 |

Wall time and child resource usage were collected with Python's standard-library
`time.monotonic` and `resource.getrusage(RUSAGE_CHILDREN)`. Peak RSS is the largest
child, not the concurrent process-tree total; block counters are kernel accounting,
not physical disk throughput. This is one paired sample on a shared machine with
substantial background I/O pressure. File-cache residency was uncontrolled, so the
block-input difference is not attributed to the fix. The before case stopped at
the expected new assertion; the after case also ran its remaining NUL presses and
clean-exit assertions. The observed wall-time reduction is 375.944 s for this
command, not a measured speedup for compilation or the whole CI pipeline.

The empty-target PTY build took 203.278 s separately. A shared sccache 0.18.0
wrapper was enabled; its initial contents were not recorded, so this was not a
verified fully cold compiler-cache run. The fixed rebuild reused the target's
dependencies; no paired cold-build improvement is claimed. To reproduce,
build with `cargo nextest run --test tui_e2e --no-run`, then time the same warm
command on each tree. Use `cargo nextest list --test tui_e2e --message-format json`
to compare the test names and ignored flags as well as the counts.

For context, the [successful reference CI run](https://github.com/Thurbeen/thurbox/actions/runs/39320327377)
at `18539d7b` had these costs. It precedes the dependency update at `206ae013` and
is not the paired baseline above:

| Job | Job duration | Dominant work |
|-----|--------------|---------------|
| Linux Nextest | 931 s | 186 s build, 708.288 s tests |
| Windows tests | 695 s | 363 s build, 223.600 s tests |
| Nix package | 808 s | 571 s flake check, 212 s cache post-step |
| Binary Size | 167 s | 141 s release build |
| Windows clippy | 116 s | 48 s clippy, toolchain and cache setup |
| Linux clippy / ARM clippy / docs | 32 / 37 / 31 s | Separate checks on separate runners |

Linux Nextest restored an exact 453,476,928-byte dependency cache; Windows tests
restored an exact 386,528,910-byte cache. The pinned rust-cache action keys by job,
toolchain and Cargo inputs and excludes workspace artifacts by default, so these
hits do not remove application compilation and linking. No cache-key or trust
policy change was needed. PTY tests accounted for 618.673 s of cumulative passing
test-process duration in that Linux run, making negotiation the first optimization.
Windows also had a long real-SSH silent-host probe; its deadline and cleanup need
separate investigation rather than a shorter timeout.

The responder optimization retains the gate commands, features, timeouts,
assertions, CI selectors and platform lanes: Linux x86-64 and ARM checks, native
Windows checks/tests, Nix,
security, docs, shell, Lua, website, installer/packaging probes and SonarQube.
The only nextest configuration change in that work corrected a comment; scheduling
changed later (see CI cache scope and test scheduling).
The two added cases cover fragmented replies and the silent-terminal fallback.
The separately integrated refusal fixture from PR #1539 uses a closed input
channel with a still-live program to check error forwarding; the existing
non-draining-receiver unit test retains deterministic full-channel coverage.
Native Windows runtime evidence comes from CI; these local measurements are Unix
PTY measurements.

### CI cache scope and test scheduling

A cache saved by a `pull_request` run is scoped to that PR's merge ref, so no
other PR can restore it. CI also runs on push to `main`, which is the only
scope every PR can read, and only `main` saves the Rust caches. A cold build
takes about twice as long as a warm one, and pull-request saves would keep the
repository over its 10 GB cache quota.
A new push to a pull request cancels its running CI run. A run on `main` is
never cancelled: a cancelled run skips its cache save.

`.config/nextest.toml` runs `tui_e2e` in the `e2e-pty` group, one test at a
time. Its tests wait on wall-clock time, and at full parallelism they starve
each other and miss their timeouts. Other binaries still run beside the group.
`a_task_dispatched_beside_running_tasks` gets a longer kill timeout: on a
loaded runner its twelve Task starts can outlast the default, which would end
the test before it names the late Task.

The CI nextest profile gives two binaries a priority, so both start first.
`tui_e2e` runs on one thread (the `e2e-pty` group), so its roughly 370 s of
tests form the longest chain in the run; started last, it would finish as a
single-threaded tail. `host_probe_deadline` takes about 135 s on Windows, and
the run would otherwise end waiting on it alone.

### Which tests a change runs

The `changes` job in `.github/workflows/ci.yml` sorts a pull request's paths
into filters, and each job runs only for its own. A change can break a test
in another component's tree, so the `Nextest` job also runs a few thurbox
tests for changes that are not thurbox's:

| Change | Runs in `Nextest` |
|--------|-------------------|
| thurbox, `runtime-mux`, `term-stream`, `workspace-git` (the `rust` filter) | the whole suite except Fleet's crates, then the doctests |
| Fleet only (the `fleet` filter) | `fleet_engine_e2e` and `tui_e2e`, against the `fleet`, `runtime-mux` and `workspace-git` it builds |
| documents or examples only (the `doc_refs` filter) | `doc_references` |

Fleet's own suite runs in the `fleet` job and in `Windows (tests)`, and its
clippy in `fleet` and `Windows (clippy)`.

A job gates on a job-level `if:`, not on a workflow-level `paths-ignore:`. A
skipped job reports `skipped`, which `All Checks` counts as a pass, so the
required gate always reports. A filter lists every input of its jobs, not only
the tree it names:

- every filter lists `ci.yml`, so a workflow edit re-runs every job;
- `rust` lists `ui/` and the embedded examples: the binary embeds them and
  the suite asserts on their content;
- `rust` lists the configs that decide its verdicts (`clippy.toml`,
  `deny.toml`, `.config/nextest.toml`) and the vendored crossterm;
- `fleet` lists every crate's manifest, the skills and `cog.toml`, which its
  tests read;
- `pr_title` lists `cog.toml`, which declares the types and scopes the checker
  enforces;
- `release_line` lists `src/update/version_check.rs`, which holds the
  constant the gate reads;
- `nix` lists every input of the thurbox build, so a new `include_str!` outside
  the package's source set fails there.

### CI jobs

- No job runs `cargo check`: `clippy --all-targets -D warnings` type-checks
  the same targets.
- `Clippy (aarch64 Linux)` runs natively. The release ships an aarch64 Linux
  archive, so an arch-only break would block it.
- `Windows (clippy)` and `Windows (tests)` run in parallel: clippy-driver and
  rustc share no artifacts, so one job would build twice. Each excludes the
  build directories from Defender real-time scanning. The local Windows VM
  (`scripts/dev/e2e/windows-vm.sh test-suite`) runs the same suite.
- `install-script-ps` runs on Linux and on Windows, in both PowerShell 7 and
  Windows PowerShell 5.1: the installer must work on the PowerShell Windows
  ships, and only Windows reproduces an update over a running `thurbox.exe`.
- `Clean install` packs the release archive with the release script and
  installs it with the installer into an empty home, in thurbox mode and in
  fleet-only mode. On Linux it installs the static musl build in a bare Debian
  container. It uses a debug build: it tests the packing and the install.
- `PR Title Checker` runs the checker's suite in `ci.yml`, not in
  `pr-title.yml`. `pr-title.yml` re-runs on every edit of the title and stays
  lean.
- `Large Files` runs on every pull request. It runs only prek's builtin hook:
  the other hooks are `language: system` and need their jobs' toolchains. The
  hook caps a file at 1 MiB and weighs modified files too: every committed
  version stays in the history each clone downloads.
- `SonarQube` skips a pull request from a fork, which gets no `SONAR_TOKEN`.
  The fork's code is analysed after it lands, when `sonar.yml` analyses
  `main`.
- `Binary Size` is informational. It is not in `All Checks`, so it never
  blocks a merge.
- The `fleet` job excludes `e2e_runtime_exec`. `Nextest` runs it, because it
  has tmux, jq and `runtime-mux`.

### Debug information and disk usage

The dev profile uses `debug = "line-tables-only"`; the test profile inherits it,
including dependency builds. Backtraces retain function names and file/line
locations, while debugger inspection of local variables and types needs full
debug information. Optimization, debug assertions, overflow checks and
incremental compilation keep their existing settings. Release profiles are
unchanged.

Measured on x86-64 Linux with Rust 1.96.0 and locked dependencies: equivalent
clean `cargo test --locked --test build_artifacts --test render_props --no-run`
builds before and after the profile change, with incremental compilation enabled.
This sample includes Cargo's application binaries, not the whole test suite.

| Artifact | Full debug (before) | Line tables (after) |
|----------|---------------------|---------------------|
| `render_props` executable | 315,447,136 B | 111,139,288 B |
| Its `.debug_*` sections | 305,365,470 B | 101,060,143 B |
| `thurbox-cli` executable | 320,331,016 B | 122,324,640 B |
| Sample `target/` disk usage | 3,571,748,864 B | 2,051,538,944 B |
| Sample `target/debug/incremental/` disk usage | 893,779,968 B | 524,468,224 B |

This applies to `cargo build`, `cargo test`, `just test`, nextest and its archives
on Linux and native Windows. Nextest's `--profile ci` selects runner settings
from `.config/nextest.toml`, not a Cargo build profile.

Request full debug information for a debugging session with environment
overrides (the default-artifact regression skips these explicit overrides):

```bash
CARGO_PROFILE_DEV_DEBUG=2 cargo build
CARGO_PROFILE_TEST_DEBUG=2 cargo test
```

In PowerShell, set `$env:CARGO_PROFILE_DEV_DEBUG = '2'` before `cargo build`, or
`$env:CARGO_PROFILE_TEST_DEBUG = '2'` before `cargo test`, then remove that variable
with `Remove-Item Env:CARGO_PROFILE_DEV_DEBUG` or
`Remove-Item Env:CARGO_PROFILE_TEST_DEBUG` when finished.

Changing profiles does not remove old artifacts. Each worktree retains its own
`target/`, including previous hash generations and incremental caches. Run
`cargo clean` in that worktree to reclaim them when a rebuild is acceptable.
For disposable builds, `CARGO_INCREMENTAL=0` avoids the incremental cache; leave
it enabled for normal development. Build output belongs on a disk with enough
space, rather than a memory-backed temporary directory.

## 3. Runtime sandbox — run thurbox isolated

The sandbox runs the dev build (`0.0.0-dev` → `dev_build` cfg, which uses a
`thurbox-dev` tmux socket) with **thurbox's own config/data redirected** into the
sandbox (via `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR`), so it never touches your
real `~/.config/thurbox` or sessions. It **keeps your real `HOME`**, so your
authenticated agent CLIs work normally — and it puts the dev `target/debug`
first on `PATH`, so `fleet` and `runtime-mux` are this checkout's, and the
sandbox gets a Fleet of its own. The tmux socket is
scoped twice over: a private `TMUX_TMPDIR`, and `THURBOX_SOCKET` naming the
server outright — a relocated `THURBOX_DATA_DIR` derives a socket of its own
(`docs/thurbox/config.md` → Relocating an instance), and teardown kills the socket by
name.

```bash
scripts/dev/sandbox.sh                 # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh         # throwaway env, wiped on exit
scripts/dev/sandbox.sh --profile foo   # a named persistent profile
scripts/dev/sandbox.sh --isolate-home  # full hermetic isolation (fresh HOME; agents have NO creds)
scripts/dev/sandbox.sh --demo          # seed a demo repo + sessions (scripts/dev/demo-repo.sh), then launch
scripts/dev/sandbox.sh --demo-big      # …and a 400-file repo whose diff is past the 4 MiB cap
scripts/dev/sandbox.sh --shell         # a shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- config show  # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean [name]  # kill + wipe a persistent profile
```

Or via `just`: `just sandbox`, `just sandbox-fresh`, `just sandbox-demo`,
`just sandbox-demo-big`, `just sandbox-shell`, `just sandbox-clean [profile]`.

The sandbox points `THURBOX_CONFIG_DIR` at its own root, so the interface
materialises at `<sandbox>/thurbox-config/ui/` along with its settings and
state. `--fresh` is therefore a clean first-run interface every time — which is
how the mod lifecycle (delivery, removal, restore) is exercised without touching
your real one.

The TUI is launched **from the sandbox root rather than the repo**.

To run the dev TUI against **this checkout's** `ui/` instead of a copy, ask for it:
`just tui-ui` (which sets `THURBOX_UI_DIR`).

**Isolation flavors:**

- **thurbox-only (default)** — real `HOME`/agents; only `thurbox-config` +
  `thurbox-data` (+ a private `TMUX_TMPDIR`) are redirected. Use this to dev with
  your real, logged-in agents without polluting your real thurbox state.
- **full (`--isolate-home`)** — also overrides `HOME` + `XDG_*`, so the env is
  hermetic and agents boot with no credentials. `scripts/demo/record-tutorial.sh`
  uses it (via `tbx_sandbox_init_full`); `record-hero.sh` sets its own `HOME`,
  and `record-doom.sh` keeps the real `HOME`.

**Profile lifetimes:**

- **Persistent** profiles live under `target/dev-sandbox/<profile>/` (gitignored;
  `cargo clean` or `--clean` removes them). Their tmux socket dir is kept short
  under `$XDG_RUNTIME_DIR` (AF_UNIX socket paths are length-limited, and the
  repo's `target/` path is often too long). Sessions survive across runs.
  The profile's Fleet and runtime-mux configs are generated on every run, so
  a profile an older checkout made never keeps a section the current `fleet`
  refuses; a TUI or shell launch stops the `fleet up` an earlier run left
  alive, and starts this build's (`tbx_sandbox_fleet_up`): thurbox never
  starts one.
- **Fresh** (`--fresh`) is a `mktemp` dir wiped on exit — same isolation
  `record-tutorial.sh` uses.

The isolation logic is one helper, `scripts/dev/lib/sandbox-env.sh`, sourced by
`scripts/dev/sandbox.sh` and the other scripts that build a sandbox (one source
of truth).
`tests/tui_e2e.rs` isolates the same way in Rust — private profile dirs, a short
private `TMUX_TMPDIR` — so it never touches a real profile either.

### Example: drive a sandbox session from its shell

```bash
scripts/dev/sandbox.sh --shell
# inside the sandbox shell (fleet/thurbox-cli target the sandbox's own Fleet):
fleet list task                       # every session is a Fleet Task
fleet send <task> "hello"             # type into its agent
fleet get task <task> | jq .status    # one Task, as JSON
```

## 4. Cargo profiles and manifests

The dev profile builds thurbox at `opt-level = 1`. The interface runs Lua on
every frame, and at `opt-level = 0` the TUI lags. Dependencies build at
`opt-level = 3` even in a dev build: they compile once, and mlua, ratatui and
vt100 run on every frame.

`term-stream` and `otel-env` keep thurbox's edition and MSRV, because thurbox
and `runtime-mux` depend on them. `rust-version` in `crates/thurbox/Cargo.toml`
is the oldest Rust the locked dependencies build on. Clippy reads its `msrv`
from there, and `tests/doc_references.rs` holds every stated version to it.

`[patch.crates-io]` in the root `Cargo.toml` replaces two published crates with
copies under `vendor/`:

- `crossterm` 0.29.0 consumes a terminal reply behind the `>` marker, such as
  the secondary device attributes `ESC [ > 0 ; 10 ; 1 c`. Unpatched, the focused
  agent receives it as the keys `0;10;1c`. The patch also rearms Mio readiness
  between reads, so input longer than one buffer arrives. The parts marked
  `thurbox patch` are the only changes. The vendored crate is not a workspace
  member: CI runs its tests from its own manifest and `Cargo.lock`.
- `tui-term` 0.3.4 requires vt100 0.16. The patch moves it to vt100 0.18.

Drop a patch when a release fixes its behaviour. `tests/tui_e2e.rs` covers
device-attributes replies and a mouse burst across the buffer edge.

`deny.toml` allows two licenses beyond the usual permissive set. `CC0-1.0` is
`notify`'s public-domain dedication. `MIT-0` is `borrow-or-share`, which Fleet
reaches through `jsonschema`; it has no attribution condition.
