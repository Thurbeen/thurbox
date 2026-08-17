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
    selene ui docs/examples
    stylua --check ui docs/examples
    # Absolute path required: a relative --configpath resolves against the
    # server's install dir, and a missed config reports every injected global as
    # undefined instead of erroring.
    lua-language-server --check ui --configpath "{{ justfile_directory() }}/.luarc.json" --checklevel=Warning

# Format the Lua interface in place (the counterpart to `cargo fmt`).
fmt-lua:
    stylua ui docs/examples

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

# Run the dev TUI in the persistent default sandbox.
sandbox *ARGS:
    scripts/dev/sandbox.sh {{ARGS}}

# Run the dev TUI in a throwaway sandbox (wiped on exit).
sandbox-fresh:
    scripts/dev/sandbox.sh --fresh

# Run the v2 kernel (thurbox2) in a throwaway sandbox (fresh interface each time).
sandbox-v2:
    scripts/dev/sandbox.sh --v2 --fresh

# Drop into a shell with the sandbox env (run `thurbox-cli …` by hand).
sandbox-shell:
    scripts/dev/sandbox.sh --shell

# Wipe a persistent sandbox profile (default: "default").
sandbox-clean PROFILE="default":
    scripts/dev/sandbox.sh --clean {{PROFILE}}

# Black-box TUI smoke test (real binary in a throwaway tmux pane).
smoke:
    scripts/dev/smoke/tui-smoke.sh

# Drive tests against a real SSH host: `just lab <host> <verb>`.
lab HOST *ARGS:
    scripts/dev/e2e/real-host.sh {{HOST}} {{ARGS}}
