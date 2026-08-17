#!/usr/bin/env bash
#
# Run a thurbox *dev build* in an isolated sandbox — one command to launch the
# `thurbox-dev` TUI/CLI against a throwaway or persistent environment that never
# touches your real ~/.config/thurbox or tmux server.
#
# By default only *thurbox's own* config/data are redirected (via THURBOX_*_DIR)
# — your real HOME/agents stay intact, so authenticated claude/codex/antigravity work.
# Pass --isolate-home for a fully hermetic env (fresh HOME, agents boot without
# credentials), e.g. to reproduce the demo/smoke conditions.
#
# Usage:
#   scripts/dev/sandbox.sh                 # persistent "default" profile, launch the TUI
#   scripts/dev/sandbox.sh --v2            # launch the v2 kernel (thurbox2) instead
#   scripts/dev/sandbox.sh --fresh         # throwaway env, wiped on exit
#   scripts/dev/sandbox.sh --profile foo   # named persistent profile
#   scripts/dev/sandbox.sh --isolate-home  # full isolation (fresh HOME; no agent creds)
#   scripts/dev/sandbox.sh --shell         # drop into a shell with the sandbox env
#                                          #   (run `thurbox-cli ...` against the sandbox DB)
#   scripts/dev/sandbox.sh -- session list # run `thurbox-cli <args>` in the sandbox
#   scripts/dev/sandbox.sh --clean [name]  # kill + wipe a persistent profile, then exit
#
# --v2 launches `thurbox2` FROM THE SANDBOX ROOT rather than the repo, because
# thurbox2 prefers a `./ui` in the working directory over the user's own copy —
# started from the repo it would load the repo's `ui/`, so the sandbox would
# isolate the database and not the interface. From the sandbox root it
# materializes `<sandbox>/thurbox-config/ui/` instead, which is what `--fresh`
# then gives you a clean one of.
#
# State (persistent mode): target/dev-sandbox/<profile>/ (gitignored). Sessions
# survive across runs — its tmux-dev server is left alive on exit. `--clean`
# (or `cargo clean`) removes it.
#
# Requires: cargo, tmux >= 3.2. Build happens before any HOME override so cargo
# still resolves your real ~/.cargo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export TBX_REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)" # consumed by the helper
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib/sandbox-env.sh"

# Logs go to stderr so `--` CLI passthrough keeps stdout clean (e.g. `--json`).
log() { printf '\033[1;36m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

mode="persistent"
profile="default"
isolation="thurbox" # thurbox | full
action="tui"        # tui | shell | cli | clean
kernel="v1"         # v1 (thurbox) | v2 (thurbox2)
cli_args=()

while [ $# -gt 0 ]; do
    case "$1" in
        --fresh) mode="fresh"; shift ;;
        --profile) profile="${2:?--profile needs a name}"; shift 2 ;;
        --isolate-home) isolation="full"; shift ;;
        --v2) kernel="v2"; shift ;;
        --shell) action="shell"; shift ;;
        --clean) action="clean"; shift; case "${1:-}" in ""|-*) ;; *) profile="$1"; shift ;; esac ;;
        --) shift; action="cli"; cli_args=("$@"); break ;;
        -h|--help) sed -n '2,35p' "$0"; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

command -v cargo >/dev/null || die "cargo not found"

if [ "$action" = "clean" ]; then
    log "cleaning sandbox profile '$profile'"
    tbx_sandbox_clean "$profile"
    exit 0
fi

# Build the dev binaries BEFORE the HOME override (so cargo finds ~/.cargo).
# thurbox-cli is built either way: it drives the same database and is what an
# agent hook inside a session calls.
if [ "$kernel" = "v2" ]; then
    tui_binary="thurbox2"
else
    tui_binary="thurbox"
fi
log "building $tui_binary + thurbox-cli (dev)"
( cd "$TBX_REPO_ROOT" && cargo build --bin "$tui_binary" --bin thurbox-cli >&2 )

if [ "$isolation" = "full" ]; then
    tbx_sandbox_init_full "$mode" "$profile"
else
    tbx_sandbox_init "$mode" "$profile"
fi
export TBX_IN_SANDBOX="$profile"

log "sandbox root: $TBX_SANDBOX_ROOT ($mode, $isolation isolation)"

# What to run in the sandbox env.
run_in_sandbox() {
    case "$action" in
        shell)
            log "entering sandbox shell — \`$tui_binary\`/\`thurbox-cli\` target this sandbox; exit to leave"
            "${SHELL:-bash}" -i
            ;;
        cli) "$TBX_REPO_ROOT/target/debug/thurbox-cli" "${cli_args[@]}" ;;
        # Run from the sandbox root, not the repo: see the --v2 note in the
        # header. Harmless for v1, which resolves no paths from the cwd.
        tui) ( cd "$TBX_SANDBOX_ROOT" && "$TBX_REPO_ROOT/target/debug/$tui_binary" ) ;;
    esac
}

# Run as a child (NOT exec): a `fresh` sandbox needs the teardown trap to fire on
# exit to reap its throwaway dir + tmux server, and `exec` would discard the
# trap. Persistent sandboxes have nothing to tear down.
[ "$TBX_SANDBOX_FRESH" = "1" ] && trap tbx_sandbox_teardown EXIT INT TERM
run_in_sandbox
