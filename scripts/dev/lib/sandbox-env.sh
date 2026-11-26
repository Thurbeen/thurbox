# shellcheck shell=sh

: "${TBX_REPO_ROOT:=${REPO_ROOT:-}}"
if [ -z "$TBX_REPO_ROOT" ]; then
    echo "sandbox-env.sh: set REPO_ROOT (or TBX_REPO_ROOT) before sourcing" >&2
    # `return` fails outside a sourced file, so `exit` covers running this directly.
    # shellcheck disable=SC2317
    return 2 2>/dev/null || exit 2
fi
export TBX_REPO_ROOT

export TBX_DEV_SOCKET="thurbox-dev"

export TBX_SANDBOX_ROOT=""
export TBX_SANDBOX_FRESH=0

tbx_sandbox_tmux_dir() {
    echo "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/thurbox-sbx-${1:-default}"
}

_tbx_name_socket_over_any_inherited_one() {
    THURBOX_SOCKET="$TBX_DEV_SOCKET"
    export THURBOX_SOCKET
    unset THURBOX_SOCKET_FOR
}

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
            # AF_UNIX socket paths are length-limited, and the deep target/ path overflows them.
            TMUX_TMPDIR="$(tbx_sandbox_tmux_dir "$profile")"
            ;;
        *)
            echo "sandbox init: unknown mode '$mode' (want fresh|persistent)" >&2
            return 2
            ;;
    esac

    export TMUX_TMPDIR
    mkdir -p "$TMUX_TMPDIR"
    _tbx_name_socket_over_any_inherited_one
    PATH="$TBX_REPO_ROOT/target/debug:$PATH"
    export PATH
}

tbx_sandbox_init() {
    _tbx_resolve_root "$@" || return $?
    THURBOX_CONFIG_DIR="$TBX_SANDBOX_ROOT/thurbox-config"
    THURBOX_DATA_DIR="$TBX_SANDBOX_ROOT/thurbox-data"
    export THURBOX_CONFIG_DIR THURBOX_DATA_DIR
    mkdir -p "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR"
}

_tbx_drop_inherited_thurbox_dir_overrides() {
    unset THURBOX_CONFIG_DIR THURBOX_DATA_DIR
}

tbx_sandbox_init_full() {
    _tbx_resolve_root "$@" || return $?
    _tbx_drop_inherited_thurbox_dir_overrides
    HOME="$TBX_SANDBOX_ROOT/home"
    XDG_CONFIG_HOME="$TBX_SANDBOX_ROOT/config"
    XDG_DATA_HOME="$TBX_SANDBOX_ROOT/data"
    XDG_STATE_HOME="$TBX_SANDBOX_ROOT/state"
    XDG_CACHE_HOME="$TBX_SANDBOX_ROOT/cache"
    export HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME
    mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
        "$XDG_CACHE_HOME"
}

_tbx_replace_by_rename_from_stdin() {
    staged="$(mktemp "$1.XXXXXX")"
    cat > "$staged"
    mv -f "$staged" "$1"
}

tbx_sandbox_fleet() {
    tbx_sandbox_runtime
    fleet_dir="$TBX_SANDBOX_ROOT/fleet"
    FLEET_CONFIG="$fleet_dir/config.toml"
    export FLEET_CONFIG
    mkdir -p "$fleet_dir"
    _tbx_replace_by_rename_from_stdin "$FLEET_CONFIG" <<'EOF'
version = 1

[data]
dir = "store"

[runtimes.mux]
program = "runtime-mux"
callTimeoutMs = 10000
keyPrefix = "tbx-sandbox"
EOF
}

tbx_sandbox_runtime() {
    RUNTIME_MUX_CONFIG="$TBX_SANDBOX_ROOT/fleet/runtime-mux.toml"
    export RUNTIME_MUX_CONFIG
    mkdir -p "$TBX_SANDBOX_ROOT/fleet"
    printf "[workspace]\nroot = '%s'\n" "$TBX_SANDBOX_ROOT/fleet/workspaces" \
        | _tbx_replace_by_rename_from_stdin "$RUNTIME_MUX_CONFIG"
}

tbx_fleet_task_name() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
        | sed 's/[^a-z0-9][^a-z0-9]*/-/g; s/^-//; s/-$//'
}

# MUST KILL: rearchitect — a JSON encoder would make a name or dir holding quotes or backslashes safe
tbx_fleet_session() {
    task="$(tbx_fleet_task_name "$1")"
    fleet get task "$task" >/dev/null 2>&1 && return 0
    case "$2" in
        *://*) url="$2" ;;
        *) url="file://$2" ;;
    esac
    printf '{"apiVersion":"fleet/v1alpha1","kind":"Task","metadata":{"name":"%s","labels":{"thurbox.dev/name":"%s"}},"spec":{"harness":"%s","host":"local","workspace":["%s"],"dispatch":true}}\n' \
        "$task" "$1" "${3:-claude-code}" "$url" | fleet apply --create-only -f - >/dev/null
}

_tbx_fleet_is_ours() {
    [ -r "/proc/$1/environ" ] || return 0
    _tbx_env=$(tr '\0' '\n' < "/proc/$1/environ")
    if printf '%s\n' "$_tbx_env" | grep -q '^FLEET_CONFIG='; then
        printf '%s\n' "$_tbx_env" | grep -qxF "FLEET_CONFIG=$2/fleet/config.toml"
        return
    fi
    _tbx_xdg=$(printf '%s\n' "$_tbx_env" | sed -n 's/^XDG_CONFIG_HOME=//p' | head -n 1)
    [ -n "$_tbx_xdg" ] || return 1
    _tbx_xdg=$(readlink -f "$_tbx_xdg" 2>/dev/null || printf '%s' "$_tbx_xdg")
    _tbx_root=$(readlink -f "$2" 2>/dev/null || printf '%s' "$2")
    case "$_tbx_xdg/" in
        "$_tbx_root"/*) return 0 ;;
        *) return 1 ;;
    esac
}

tbx_sandbox_fleet_up() {
    _tbx_stop_fleet "$TBX_SANDBOX_ROOT" || return 1
    mkdir -p "$TBX_SANDBOX_ROOT/fleet"
    nohup fleet up >"$TBX_SANDBOX_ROOT/fleet/up.log" 2>&1 </dev/null &
    echo "$!" >"$TBX_SANDBOX_ROOT/fleet/up.pid"
}

_tbx_stop_fleet() {
    stopped=0
    if [ -f "$1/fleet/up.pid" ]; then
        if _tbx_stop_manager "$(cat "$1/fleet/up.pid")" "$1"; then
            rm -f "$1/fleet/up.pid"
        else
            stopped=1
        fi
    fi
    while read -r log; do
        pid="${log##*/fleet-up-}"
        _tbx_stop_manager "${pid%.log}" "$1" || stopped=1
    done <<EOF
$(find "$1" -name 'fleet-up-*.log' 2>/dev/null)
EOF
    return "$stopped"
}

_tbx_stop_manager() {
    case "$1" in
        *[!0-9]*|"") return 0 ;;
    esac
    case "$(ps -p "$1" -o args= 2>/dev/null)" in
        *fleet" up") ;;
        *) return 0 ;;
    esac
    _tbx_fleet_is_ours "$1" "$2" || return 0
    kill "$1" 2>/dev/null || return 0
    _tbx_wait_gone "$1" "${TBX_FLEET_STOP_TIMEOUT:-120}" && return 0
    echo "sandbox: fleet up $1 did not stop within ${TBX_FLEET_STOP_TIMEOUT:-120} s" >&2
    return 1
}

_tbx_wait_gone() {
    ticks=$(($2 * 10))
    while [ "$ticks" -gt 0 ]; do
        case "$(ps -p "$1" -o stat= 2>/dev/null)" in
            ""|Z*) return 0 ;;
        esac
        sleep 0.1
        ticks=$((ticks - 1))
    done
    return 1
}

_tbx_kill_servers() {
    for socket in "$TBX_DEV_SOCKET" runtime-mux; do
        tmux -L "$socket" kill-server >/dev/null 2>&1 || true
    done
}

tbx_sandbox_teardown() {
    [ -n "$TBX_SANDBOX_ROOT" ] && _tbx_stop_fleet "$TBX_SANDBOX_ROOT"
    _tbx_kill_servers
    if [ "$TBX_SANDBOX_FRESH" = "1" ] && [ -n "$TBX_SANDBOX_ROOT" ]; then
        rm -rf "$TBX_SANDBOX_ROOT"
    fi
}

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
