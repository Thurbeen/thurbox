# thurbox dev task runner. Run `just` (or `just --list`) to see tasks.
#
# Enter the pinned toolchain first with `nix develop` (or `direnv allow`); these
# tasks assume the dev tools (cargo-nextest, cargo-deny, rumdl, shellcheck, selene,
# stylua, …)
# are on PATH. See docs/DEVELOPMENT.md.

# Default: show the task list.
default:
    @just --list

# Type-check everything.
check:
    cargo check --all

# Build the dev binaries (TUI + CLI).
build:
    cargo build --bin thurbox --bin thurbox-cli

# Run the full test suite (nextest).
test:
    cargo nextest run --all

# Run a single test by name: `just test-one perf_`.
test-one NAME:
    cargo nextest run -E 'test({{NAME}})'

# Run the bats suites: the install scripts, the commit-history checker and the
# extensions' shell scripts. Not part of `just test` (which is cargo's), and
# needs bats + jq on PATH (the checker's suite skips without `cog`).
test-scripts:
    bats scripts/install.bats
    bats scripts/ci/check-conventional-commits.bats
    bats extensions/*/scripts/*.bats

# Format Rust + website code.
fmt:
    cargo fmt --all
    npm run fmt:website

# Lint everything CI lints (Rust + deny + markdown + shell + Lua).
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo deny check advisories
    cargo deny check bans licenses sources
    rumdl check .
    git ls-files -z '*.sh' | xargs -0 shellcheck
    selene ui examples
    stylua --check ui examples
    # Absolute path required: a relative --configpath resolves against the
    # server's install dir, and a missed config reports every injected global as
    # undefined instead of erroring.
    lua-language-server --check ui --configpath "{{ justfile_directory() }}/.luarc.json" --checklevel=Warning

# Format the Lua interface in place (the counterpart to `cargo fmt`).
fmt-lua:
    stylua ui examples

# Architecture-rule + doc checks.
arch:
    cargo test --test architecture_rules
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

# Install the non-Nix dev tools (fallback when not using the flake).
dev-tools:
    scripts/install-dev-tools.sh

# Install the prek git hooks.
hooks-install:
    prek install

# `cargo run` alone reads `~/.config/thurbox-dev/ui`, like every other config a dev
# build reads. Editing the interface in the repository is a different request, so it
# is stated here rather than inferred from the working directory.
#
# Run the dev TUI against THIS checkout's ui/ instead of your own copy.
tui-ui *ARGS:
    THURBOX_UI_DIR="{{justfile_directory()}}/ui" cargo run --bin thurbox -- {{ARGS}}

# Run the dev TUI in the persistent default sandbox.
sandbox *ARGS:
    scripts/dev/sandbox.sh {{ARGS}}

# The interface lives inside the sandbox root, so a throwaway sandbox is already a
# clean interface directory. There is no separate recipe for that; there was one
# (`sandbox-fresh-ui`) which ran exactly this and claimed to do something else.

# Run the dev TUI in a throwaway sandbox (wiped on exit).
sandbox-fresh:
    scripts/dev/sandbox.sh --fresh

# A bare `sandbox` deliberately starts empty — the state most bugs are reported
# against. These opt in: one repository with a file of each git status, a session
# whose branch has changes, and one whose branch deliberately has none. Idempotent,
# so running either again costs a few lookups and changes nothing.

# Seed a demo repository + sessions into the persistent sandbox, then launch.
sandbox-demo:
    scripts/dev/sandbox.sh --demo

# As sandbox-demo, plus a 400-file repository whose diff is past the 4 MiB cap.
sandbox-demo-big:
    scripts/dev/sandbox.sh --demo-big

# Drop into a shell with the sandbox env (run `thurbox-cli …` by hand).
sandbox-shell:
    scripts/dev/sandbox.sh --shell

# Wipe a persistent sandbox profile (default: "default").
sandbox-clean PROFILE="default":
    scripts/dev/sandbox.sh --clean {{PROFILE}}

# Black-box TUI test: the real binary on a real pty (tests/tui_e2e.rs).
smoke:
    cargo nextest run --test tui_e2e

# Drive tests against a real SSH host: `just lab <host> <verb>`.
lab HOST *ARGS:
    scripts/dev/e2e/real-host.sh {{HOST}} {{ARGS}}
