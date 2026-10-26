# Development

How to set up a reproducible thurbox dev environment and run the app in an
isolated sandbox.

## 1. Toolchain — the dev environment

### Recommended: Nix flake

The `flake.nix` provides the tools CI uses — the Rust toolchain (read from
`rust-toolchain.toml`), interactive Bash, `tmux`, `shellcheck`, `bats`, Node,
`cargo-nextest`, `cargo-deny`, `cocogitto`, `just`, and the demo stack
(`vhs`/`ffmpeg`/`ttyd`). Interactive Bash is explicit because the real-pane
tests exercise Readline's bracketed-paste mode; Nix's stdenv Bash omits those
interactive builtins.
`flake.lock` pins nixpkgs, so the shell's tools move only when someone runs
`nix flake update`; the Rust toolchain still follows `stable` from
`rust-toolchain.toml`. The same flake also packages thurbox itself
(`nix/package.nix`), which CI's `nix` job builds; the rest of CI installs its
tools without Nix, so the shell is a local convenience, not what CI runs.

```bash
# one-time, if not done already: enable flakes
#   mkdir -p ~/.config/nix && echo 'experimental-features = nix-command flakes' >> ~/.config/nix/nix.conf

nix develop           # enter the pinned shell
# ...or, with direnv installed, once:
direnv allow          # auto-enters the shell on `cd` (see .envrc)
```

A couple of tools aren't packaged in nixpkgs yet (`prek`, `rumdl`, nightly
`cargo-pup`); the shell prints a hint to install them via
`scripts/install-dev-tools.sh`.

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
provides tmux. [CONFIG.md](CONFIG.md#multiplexer-requirements-and-rmux-setup)
records version evidence and RMUX platform limits.

## 2. Everyday tasks — `just`

`just` (in the dev shell) is the task entrypoint — run `just` for the list:

| Task | What it does |
|------|--------------|
| `just build` | build the dev binaries (`thurbox` + `thurbox-cli`) |
| `just test` | `cargo nextest run --all` |
| `just test-scripts` | the bats suites: `scripts/install.bats` + the pull-request-title checker (needs `bats`) |
| `just lint` | fmt-check + clippy + cargo-deny + rumdl + shellcheck + selene, stylua and lua-language-server |
| `just fmt` | format Rust + website |
| `just arch` | architecture-rule + rustdoc checks |
| `just hooks-install` | `prek install` |
| `just smoke` | black-box TUI test: the real binary on a pty (`tests/tui_e2e.rs`) |
| `just reap-tmux` | reap orphaned *test* tmux servers a signal left behind (below) |
| `just bench` | what a frame costs, piece by piece (`benches/frame_cost.rs`) |
| `just perf` | what the whole binary costs under load (`scripts/dev/perf-run.sh`) |
| `just sandbox*` | dev runtime sandbox (below) |

### `just reap-tmux` — the servers a guard could not reap

Every harness that starts a tmux server holds a `TmuxServer`
(`tests/support/tmux_server.rs`), whose `Drop` kills it. That covers every way
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
explained in [`docs/PERFORMANCE.md`](PERFORMANCE.md).

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
The only nextest configuration change corrects a comment; scheduling is unchanged.
The two added cases cover fragmented replies and the silent-terminal fallback.
The separately integrated refusal fixture from PR #1539 uses a closed input
channel with a still-live program to check error forwarding; the existing
non-draining-receiver unit test retains deterministic full-channel coverage.
Native Windows runtime evidence comes from CI; these local measurements are Unix
PTY measurements.

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
authenticated agent CLIs (`claude`/`codex`/`antigravity`/…) work normally — and it puts
the dev `target/debug` first on `PATH`, so an agent's status hook calls *this*
`thurbox-cli` and writes to the sandbox DB the TUI reads. The tmux socket is
scoped twice over: a private `TMUX_TMPDIR`, and `THURBOX_SOCKET` naming the
server outright — a relocated `THURBOX_DATA_DIR` derives a socket of its own
(`docs/CONFIG.md` → Relocating an instance), and teardown kills the socket by
name.

```bash
scripts/dev/sandbox.sh                 # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh         # throwaway env, wiped on exit
scripts/dev/sandbox.sh --profile foo   # a named persistent profile
scripts/dev/sandbox.sh --isolate-home  # full hermetic isolation (fresh HOME; agents have NO creds)
scripts/dev/sandbox.sh --shell         # a shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- session list # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean [name]  # kill + wipe a persistent profile
```

Or via `just`: `just sandbox`, `just sandbox-fresh`,
`just sandbox-shell`, `just sandbox-clean [profile]`.

The sandbox points `THURBOX_CONFIG_DIR` at its own root, so the interface
materialises at `<sandbox>/thurbox-config/ui/` along with agents, settings and the
database. `--fresh` is therefore a clean first-run interface every time — which is
how the plugin lifecycle (delivery, removal, restore) is exercised without touching
your real one.

The TUI is launched **from the sandbox root rather than the repo**. Where you
stand no longer decides which interface loads, so this is belt-and-braces.

To run the dev TUI against **this checkout's** `ui/` instead of a copy, ask for it:
`just tui-ui` (which sets `THURBOX_UI_DIR`).

**Isolation flavors:**

- **thurbox-only (default)** — real `HOME`/agents; only `thurbox-config` +
  `thurbox-data` (+ a private `TMUX_TMPDIR`) are redirected. Use this to dev with
  your real, logged-in agents without polluting your real thurbox state.
- **full (`--isolate-home`)** — also overrides `HOME` + `XDG_*`, so the env is
  hermetic and agents boot with no credentials. This is what `scripts/demo/
  record.sh` uses (via `tbx_sandbox_init_full`).

**Profile lifetimes:**

- **Persistent** profiles live under `target/dev-sandbox/<profile>/` (gitignored;
  `cargo clean` or `--clean` removes them). Their tmux socket dir is kept short
  under `$XDG_RUNTIME_DIR` (AF_UNIX socket paths are length-limited, and the
  repo's `target/` path is often too long). Sessions survive across runs.
- **Fresh** (`--fresh`) is a `mktemp` dir wiped on exit — same isolation the
  demo recorder uses.

The isolation logic is one helper, `scripts/dev/lib/sandbox-env.sh`, sourced by
`scripts/dev/sandbox.sh` and `scripts/demo/record.sh` (one source of truth).
`tests/tui_e2e.rs` isolates the same way in Rust — private profile dirs, a short
private `TMUX_TMPDIR` — so it never touches a real profile either.

### Example: watch a session's status hook end-to-end

```bash
scripts/dev/sandbox.sh --shell
# inside the sandbox shell (thurbox/thurbox-cli target the sandbox):
thurbox-cli session create --name demo --repo-path "$PWD" --agent claude
thurbox-cli session signal --state blocked --session <id>   # what an agent hook does
thurbox-cli session list --json | jq '.[].name'
```
