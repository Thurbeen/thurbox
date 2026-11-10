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
#                           authenticated agent CLIs (claude/codex/antigravity/…) work.
#   tbx_sandbox_init_full — *full* isolation: also overrides HOME + XDG_* under
#                           the sandbox (hermetic; agents boot with no creds).
#                           Used by the demo recorder.
#
# Source it (don't execute) AFTER setting REPO_ROOT (or TBX_REPO_ROOT). Written
# in POSIX sh so both the bash caller (sandbox.sh) and the
# /usr/bin/env sh caller (record.sh) can source it. Both flavors prepend the
# repo's target/debug to PATH, so `fleet` and `runtime-mux` resolve to this
# checkout's. Sessions are Fleet's: tbx_sandbox_fleet gives the sandbox a Fleet
# of its own, and tbx_fleet_session creates one.
#
# Build the binaries (thurbox, thurbox-cli, fleet, runtime-mux) BEFORE calling
# init so cargo still resolves your ~/.cargo (the full flavor overrides $HOME).

: "${TBX_REPO_ROOT:=${REPO_ROOT:-}}"
if [ -z "$TBX_REPO_ROOT" ]; then
    echo "sandbox-env.sh: set REPO_ROOT (or TBX_REPO_ROOT) before sourcing" >&2
    # `return` works when sourced (the intended use); `exit` covers the mistake
    # of executing this file directly.
    # shellcheck disable=SC2317
    return 2 2>/dev/null || exit 2
fi
export TBX_REPO_ROOT

# The sandbox's tmux socket name (the dev build's default — mirrors
# crates/runtime-mux/src/instance.rs TMUX_SOCKET for a dev_build). It lives inside the sandbox's
# private TMUX_TMPDIR, so killing it can never reach a real server. Exported to
# thurbox as THURBOX_SOCKET by the init flavors below: a sandbox relocates
# THURBOX_DATA_DIR, and thurbox derives a socket of its own from a relocated
# data dir, so the teardown below would otherwise be killing a name nothing
# created.
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
    # Name the socket outright (see TBX_DEV_SOCKET) so teardown/clean can find
    # it whichever flavor of isolation ran. THURBOX_SOCKET_FOR — the tag that
    # tells thurbox an injected socket from an operator's own — has to go with
    # it: a sandbox started from inside a thurbox pane inherits one naming the
    # operator's data dir, and socket_for then rules the name above inherited
    # and derives one from the sandbox's own data dir instead. The sandbox
    # would run on a server nothing here can name, and the teardown below would
    # kill an empty one — a leaked tmux server (with its agents and heartbeat)
    # per sandbox run.
    THURBOX_SOCKET="$TBX_DEV_SOCKET"
    export THURBOX_SOCKET
    unset THURBOX_SOCKET_FOR
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
# no creds). For the demo recorder. Uses the dev_build `thurbox-dev`
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

# tbx_sandbox_fleet — give the sandbox a Fleet of its own (ADR-41): every
# session is a Fleet Task, and thurbox reads them through `fleet watch` and
# starts `fleet up` itself. Without this, `fleet` would read the operator's own
# config — their real Tasks in a sandbox's interface. Exports FLEET_CONFIG and
# writes the config once, under the sandbox root:
#
#   - `runtime-mux` (target/debug, on PATH after an init) serves `local`, on
#     the sandbox's tmux directory and socket, which it inherits.
#   - `fleet up` refuses to start without a starter Task/vega. The starter
#     here is one that is never dispatched, so no agent runs for it; it does
#     show as a row named `vega`.
#
# Call it after an init flavor.
tbx_sandbox_fleet() {
    fleet_dir="$TBX_SANDBOX_ROOT/fleet"
    FLEET_CONFIG="$fleet_dir/config.toml"
    export FLEET_CONFIG
    [ -f "$FLEET_CONFIG" ] && return 0
    mkdir -p "$fleet_dir/starter"
    printf '[{"apiVersion":"fleet/v1alpha1","kind":"Task","metadata":{"name":"vega"},"spec":{"harness":"sh","host":"local","workspace":{"dir":"%s"},"dispatch":false}}]\n' \
        "$TBX_SANDBOX_ROOT" > "$fleet_dir/starter/task.json"
    cat > "$FLEET_CONFIG" <<'EOF'
version = 1

[data]
dir = "store"

[runtimes.mux]
program = "runtime-mux"
callTimeoutMs = 10000
keyPrefix = "tbx-sandbox"

[hosts.local]
runtime = "mux"

[starter]
workspace = "starter"
EOF
    # The demo repositories (`demo-repo.sh`), so a session the interface
    # creates on one gets a worktree of its own: Fleet makes worktrees of
    # mapped repositories only.
    printf '\n[workspaces.repositories]\n"https://sandbox.invalid/demo" = "%s"\n"https://sandbox.invalid/big" = "%s"\n' \
        "$TBX_SANDBOX_ROOT/repos/demo" "$TBX_SANDBOX_ROOT/repos/big" >> "$FLEET_CONFIG"
}

# tbx_fleet_task_name <name> — the Task name thurbox gives a session called
# <name>: lowercase, each run of other characters one `-`.
tbx_fleet_task_name() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
        | sed 's/[^a-z0-9][^a-z0-9]*/-/g; s/^-//; s/-$//'
}

# tbx_fleet_session <name> <dir> [harness] — an interactive session over <dir>,
# as thurbox's create flow makes one: a dispatched Task with no brief, its
# given name in the `thurbox.dev/name` label. <harness> defaults to
# claude-code; Fleet runs any other name (or an absolute path) as the program,
# with no hooks. Skips a Task that already exists. Nothing spawns until a
# `fleet up` (thurbox starts one) or a `fleet reconcile` runs. <name> and <dir>
# go into JSON as they are: keep quotes and backslashes out of them.
tbx_fleet_session() {
    task="$(tbx_fleet_task_name "$1")"
    fleet get task "$task" >/dev/null 2>&1 && return 0
    printf '{"apiVersion":"fleet/v1alpha1","kind":"Task","metadata":{"name":"%s","labels":{"thurbox.dev/name":"%s"}},"spec":{"harness":"%s","host":"local","workspace":{"dir":"%s"},"dispatch":true}}\n' \
        "$task" "$1" "${3:-claude-code}" "$2" | fleet apply --create-only -f - >/dev/null
}

# _tbx_fleet_is_ours <pid> <root> — whether that process runs on the Fleet
# config tbx_sandbox_fleet wrote under <root>. Without /proc nothing tells, so
# the caller's `fleet up` check is all there is.
_tbx_fleet_is_ours() { # <pid> <root>
    [ -r "/proc/$1/environ" ] || return 0
    tr '\0' '\n' < "/proc/$1/environ" | grep -qxF "FLEET_CONFIG=$2/fleet/config.toml"
}

# _tbx_stop_fleet <root> — stop the `fleet up` thurbox started for the sandbox
# under <root>. It runs in a process group of its own and outlives the TUI, so
# nothing else ends it. thurbox names its log after it, `fleet-up-<pid>.log` in
# its data directory: that file names the process, rather than a search for
# one. SIGTERM finishes its sweep and releases the lease.
_tbx_stop_fleet() {
    find "$1" -name 'fleet-up-*.log' 2>/dev/null | while read -r log; do
        pid="${log##*/fleet-up-}"
        pid="${pid%.log}"
        case "$pid" in
            *[!0-9]*|"") ;; # named before its manager started: no manager
            # A log can outlive its manager and the pid be reused: signal it
            # only while it is still a `fleet up`, and, where /proc shows its
            # environment, this sandbox's.
            *) case "$(ps -p "$pid" -o args= 2>/dev/null)" in
                   *fleet" up") _tbx_fleet_is_ours "$pid" "$1" \
                       && kill "$pid" 2>/dev/null || true ;;
               esac ;;
        esac
    done
}

# _tbx_kill_servers — kill the tmux servers of the private TMUX_TMPDIR in
# force: thurbox's own socket, and `runtime-mux`, the one runtime-mux keeps
# Fleet's sessions on. Safe: that directory is the sandbox's alone.
_tbx_kill_servers() {
    for socket in "$TBX_DEV_SOCKET" runtime-mux; do
        tmux -L "$socket" kill-server >/dev/null 2>&1 || true
    done
}

# tbx_sandbox_teardown — stop the sandbox's `fleet up`, kill its tmux servers
# and, for a `fresh` sandbox, remove the whole root. Persistent sandboxes are
# left intact (use tbx_sandbox_clean to wipe them).
tbx_sandbox_teardown() {
    [ -n "$TBX_SANDBOX_ROOT" ] && _tbx_stop_fleet "$TBX_SANDBOX_ROOT"
    _tbx_kill_servers
    if [ "$TBX_SANDBOX_FRESH" = "1" ] && [ -n "$TBX_SANDBOX_ROOT" ]; then
        rm -rf "$TBX_SANDBOX_ROOT"
    fi
}

# tbx_sandbox_clean [profile] — stop a persistent profile's `fleet up`, kill
# its tmux servers and remove its root + short tmux dir. Default profile
# "default".
tbx_sandbox_clean() {
    profile="${1:-default}"
    root="$TBX_REPO_ROOT/target/dev-sandbox/$profile"
    tdir="$(tbx_sandbox_tmux_dir "$profile")"
    _tbx_stop_fleet "$root"
    if [ -d "$tdir" ]; then
        for socket in "$TBX_DEV_SOCKET" runtime-mux; do
            TMUX_TMPDIR="$tdir" tmux -L "$socket" kill-server >/dev/null 2>&1 || true
        done
    fi
    rm -rf "$root" "$tdir"
}
