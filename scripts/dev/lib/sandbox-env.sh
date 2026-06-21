# shellcheck shell=sh
#
# Shared dev-sandbox isolation helper — the single source of truth for running a
# thurbox *dev build* (`0.0.0-dev` => `dev_build` cfg, which uses the
# `thurbox-dev` tmux socket) against an isolated environment, without polluting
# your real thurbox config/sessions.
#
# Two isolation flavors:
#
#   tbx_sandbox_init      — *thurbox-only* isolation (DEFAULT for sandbox.sh):
#                           redirects only THURBOX_CONFIG_DIR / THURBOX_DATA_DIR
#                           (+ TMUX_TMPDIR), leaving HOME/XDG real so your real,
#                           authenticated agent CLIs (claude/codex/gemini/…) work.
#   tbx_sandbox_init_full — *full* isolation: also overrides HOME + XDG_* under
#                           the sandbox (hermetic; agents boot with no creds).
#                           Used by the demo recorder + TUI smoke test.
#
# Source it (don't execute) AFTER setting REPO_ROOT (or TBX_REPO_ROOT). Written
# in POSIX sh so both bash callers (sandbox.sh, tui-smoke-test.sh) and the
# /usr/bin/env sh caller (record.sh) can source it. Both flavors prepend the
# repo's target/debug to PATH so an agent hook's `thurbox-cli` resolves to *this*
# dev binary (and writes to the sandbox DB the dev TUI reads).
#
# Build the binaries BEFORE calling init so cargo still resolves your ~/.cargo
# (the full flavor overrides $HOME).

: "${TBX_REPO_ROOT:=${REPO_ROOT:-}}"
if [ -z "$TBX_REPO_ROOT" ]; then
    echo "sandbox-env.sh: set REPO_ROOT (or TBX_REPO_ROOT) before sourcing" >&2
    # `return` works when sourced (the intended use); `exit` covers the mistake
    # of executing this file directly.
    # shellcheck disable=SC2317
    return 2 2>/dev/null || exit 2
fi
export TBX_REPO_ROOT

# The dev build's tmux socket name (mirrors src/agent/tmux.rs TMUX_SOCKET for a
# dev_build). It lives inside the sandbox's private TMUX_TMPDIR, so killing it
# can never reach a real server.
export TBX_DEV_SOCKET="thurbox-dev"

# Populated by an init call.
export TBX_SANDBOX_ROOT=""
export TBX_SANDBOX_FRESH=0

# tbx_sandbox_tmux_dir <profile> — short, stable tmux socket dir for a persistent
# profile (kept off the deep target/ path; AF_UNIX socket paths are length-limited).
tbx_sandbox_tmux_dir() {
    echo "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/thurbox-sbx-${1:-default}"
}

# _tbx_resolve_root <fresh|persistent> [profile] — common setup shared by both
# init flavors: resolves TBX_SANDBOX_ROOT, exports TMUX_TMPDIR (short, see above),
# and prepends target/debug to PATH. Internal.
_tbx_resolve_root() {
    mode="${1:-persistent}"
    profile="${2:-default}"

    case "$mode" in
        fresh)
            TBX_SANDBOX_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/thurbox-sandbox.XXXXXX")"
            TBX_SANDBOX_FRESH=1
            TMUX_TMPDIR="$TBX_SANDBOX_ROOT/tmux"
            ;;
        persistent)
            TBX_SANDBOX_ROOT="$TBX_REPO_ROOT/target/dev-sandbox/$profile"
            TBX_SANDBOX_FRESH=0
            mkdir -p "$TBX_SANDBOX_ROOT"
            # The deep target/ path overflows tmux's socket-path limit, so keep
            # the persistent socket dir short + stable per profile.
            TMUX_TMPDIR="$(tbx_sandbox_tmux_dir "$profile")"
            ;;
        *)
            echo "sandbox init: unknown mode '$mode' (want fresh|persistent)" >&2
            return 2
            ;;
    esac

    export TMUX_TMPDIR
    mkdir -p "$TMUX_TMPDIR"
    PATH="$TBX_REPO_ROOT/target/debug:$PATH"
    export PATH
}

# tbx_sandbox_init <fresh|persistent> [profile] — THURBOX-ONLY isolation.
# Real HOME/XDG (so authenticated agent CLIs work); only thurbox's config/data
# are redirected into the sandbox via the THURBOX_*_DIR overrides paths.rs honors.
tbx_sandbox_init() {
    _tbx_resolve_root "$@" || return $?
    THURBOX_CONFIG_DIR="$TBX_SANDBOX_ROOT/thurbox-config"
    THURBOX_DATA_DIR="$TBX_SANDBOX_ROOT/thurbox-data"
    export THURBOX_CONFIG_DIR THURBOX_DATA_DIR
    mkdir -p "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR"
}

# tbx_sandbox_init_full <fresh|persistent> [profile] — FULL isolation.
# Overrides HOME + XDG_* under the sandbox too (hermetic; agents boot fresh with
# no creds). For the demo recorder + smoke test. Uses the dev_build `thurbox-dev`
# XDG subdir (no THURBOX_*_DIR override needed).
tbx_sandbox_init_full() {
    _tbx_resolve_root "$@" || return $?
    # Hermetic: drop any inherited THURBOX_*_DIR overrides — paths.rs honors them
    # ahead of XDG, so an inherited one would silently defeat the isolation.
    unset THURBOX_CONFIG_DIR THURBOX_DATA_DIR
    HOME="$TBX_SANDBOX_ROOT/home"
    XDG_CONFIG_HOME="$TBX_SANDBOX_ROOT/config"
    XDG_DATA_HOME="$TBX_SANDBOX_ROOT/data"
    XDG_STATE_HOME="$TBX_SANDBOX_ROOT/state"
    XDG_CACHE_HOME="$TBX_SANDBOX_ROOT/cache"
    export HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME
    mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
        "$XDG_CACHE_HOME"
}

# tbx_sandbox_teardown — kill the sandbox's tmux server (safe: private
# TMUX_TMPDIR) and, for a `fresh` sandbox, remove the whole root. Persistent
# sandboxes are left intact (use tbx_sandbox_clean to wipe them).
tbx_sandbox_teardown() {
    tmux -L "$TBX_DEV_SOCKET" kill-server >/dev/null 2>&1 || true
    if [ "$TBX_SANDBOX_FRESH" = "1" ] && [ -n "$TBX_SANDBOX_ROOT" ]; then
        rm -rf "$TBX_SANDBOX_ROOT"
    fi
}

# tbx_sandbox_clean [profile] — kill a persistent profile's tmux server and
# remove its root + short tmux dir. Default profile "default".
tbx_sandbox_clean() {
    profile="${1:-default}"
    root="$TBX_REPO_ROOT/target/dev-sandbox/$profile"
    tdir="$(tbx_sandbox_tmux_dir "$profile")"
    if [ -d "$tdir" ]; then
        TMUX_TMPDIR="$tdir" tmux -L "$TBX_DEV_SOCKET" kill-server >/dev/null 2>&1 || true
    fi
    rm -rf "$root" "$tdir"
}
