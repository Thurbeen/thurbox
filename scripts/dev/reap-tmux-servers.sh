#!/usr/bin/env bash
#
# Reap the test tmux servers a run left behind — the ones a guard could not.
#
# `tests/support/tmux_server.rs` kills a harness's server in `Drop`, which
# covers every way a test can end *in-process*: returning, asserting, panicking.
# It cannot cover a signal that runs no destructor — a nextest `slow-timeout`
# termination, a `kill -9`, a Ctrl-C at the wrong moment, an OOM kill. The
# server left that way keeps its agent processes, keeps its CPU, and loses its
# socket file with the directory the run owned, so `tmux -L <name> kill-server`
# has nothing to connect to. That is how one machine reached 400 orphans,
# 1432 processes and 4.4 GiB RSS (issue #1175).
#
# This finds them by process rather than by socket, and kills only servers that
# are BOTH one of the suite's own socket names AND unreachable — a server whose
# socket file still exists is one somebody can still use, including a test
# running right now, so it is left alone.
#
# Usage:
#   scripts/dev/reap-tmux-servers.sh            # reap, reporting each one
#   scripts/dev/reap-tmux-servers.sh --dry-run  # list them and change nothing
#
# Linux only: the socket directory a server was started with is read from
# /proc/<pid>/environ, and there is no portable equivalent. On anything else it
# says so and exits 0 rather than guessing — a sweep that guessed would be a
# sweep that killed the wrong server.

set -euo pipefail

DRY_RUN=0
case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    -h | --help)
        sed -n '3,27p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    "") ;;
    *)
        echo "unknown argument: $1 (try --help)" >&2
        exit 2
        ;;
esac

# The suite's own socket names, as globs. Every harness socket in `tests/` and
# in `src/agent/control_mode/tests.rs` matches one of these, and nothing else
# does: an operator's own server is `thurbox`, a relocated instance's is
# `thurbox-dev` or `thurbox-<digest>` (ADR-12), and none of those is listed
# here. Keep it that way — a sweep wide enough to catch `thurbox-dev` is a
# sweep that kills the session this is being typed into.
SUITE_SOCKETS=(
    'thurbox-*-e2e'      # create, reap, send-keys, spawn-cmd, program-*, …
    'thurbox-e2e-*'      # tui_e2e, one per process
    'thurbox-*-test'     # attach, capture, hookstate, respawn, watch, …
    'thurbox-cm-*'       # src/agent/control_mode/tests.rs
    'thurbox-leak-*'     # tests/tmux_server_leak.rs
    'thurbox-rename-*'   # tests/session_rename.rs
    'thurbox-life-*'     # tests/session_lifetime.rs
    'thurbox-forget-*'   # tests/session_lifetime.rs
    'thurbox-stopped-*'  # tests/session_lifetime.rs
    'thurbox-panic-probe-*'
)

if [ ! -d /proc/self ]; then
    echo "reap-tmux-servers: needs /proc (Linux); nothing done." >&2
    exit 0
fi

# Is this one of the suite's socket names?
is_suite_socket() {
    local name="$1" glob
    for glob in "${SUITE_SOCKETS[@]}"; do
        # shellcheck disable=SC2053 # a glob on the right is the point here
        [[ $name == $glob ]] && return 0
    done
    return 1
}

# The `-L <name>` a tmux process was started with, or nothing.
socket_of() {
    tr '\0' '\n' < "/proc/$1/cmdline" 2> /dev/null |
        awk '$0 == "-L" { want = 1; next } want { print; exit }'
}

# The TMUX_TMPDIR a process was started with, defaulting the way tmux does.
tmpdir_of() {
    local dir
    dir=$(tr '\0' '\n' < "/proc/$1/environ" 2> /dev/null |
        sed -n 's/^TMUX_TMPDIR=//p' | head -1)
    printf '%s' "${dir:-/tmp}"
}

uid=$(id -u)
found=0
reaped=0

for proc in /proc/[0-9]*; do
    pid=${proc#/proc/}
    # tmux renames the server process; the client keeps its own argv.
    [ "$(cat "$proc/comm" 2> /dev/null)" = "tmux: server" ] || continue
    # Somebody else's server is not ours to look at, let alone kill.
    [ -O "$proc" ] || continue

    socket=$(socket_of "$pid")
    [ -n "$socket" ] || continue
    is_suite_socket "$socket" || continue

    # Reachable means usable: a live harness's server is exactly this shape.
    path="$(tmpdir_of "$pid")/tmux-$uid/$socket"
    [ -S "$path" ] && continue

    found=$((found + 1))
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "would reap pid $pid  socket $socket  (no socket at $path)"
        continue
    fi
    if kill "$pid" 2> /dev/null; then
        reaped=$((reaped + 1))
        echo "reaped pid $pid  socket $socket"
    else
        echo "could not kill pid $pid ($socket)" >&2
    fi
done

if [ "$found" -eq 0 ]; then
    echo "no orphaned test tmux servers."
elif [ "$DRY_RUN" -eq 1 ]; then
    echo "$found orphaned test tmux server(s); re-run without --dry-run to reap."
else
    echo "reaped $reaped of $found orphaned test tmux server(s)."
fi
