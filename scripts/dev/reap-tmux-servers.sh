#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Reap the test tmux servers a run left behind — the ones a guard could not.

`crates/runtime-mux/tests/support/tmux_server.rs` kills a harness's server in `Drop`, which
covers every way a test can end *in-process*: returning, asserting, panicking.
It cannot cover a signal that runs no destructor — a nextest `slow-timeout`
termination, a `kill -9`, a Ctrl-C at the wrong moment, an OOM kill. The
server left that way keeps its agent processes, keeps its CPU, and loses its
socket file with the directory the run owned, so `tmux -L <name> kill-server`
has nothing to connect to. That is how one machine reached 400 orphans,
1432 processes and 4.4 GiB RSS (issue #1175).

This finds them by process rather than by socket, and kills only servers that
are BOTH one of the suite's own socket names AND unreachable — a server whose
socket file still exists is one somebody can still use, including a test
running right now, so it is left alone.

Usage:
  scripts/dev/reap-tmux-servers.sh            # reap, reporting each one
  scripts/dev/reap-tmux-servers.sh --dry-run  # list them and change nothing

Linux only: the socket directory a server was started with is read from
/proc/<pid>/environ, and there is no portable equivalent. On anything else it
says so and exits 0 rather than guessing — a sweep that guessed would be a
sweep that killed the wrong server.

EOF
}

DRY_RUN=0
case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    -h | --help)
        usage
        exit 0
        ;;
    "") ;;
    *)
        echo "unknown argument: $1 (try --help)" >&2
        exit 2
        ;;
esac

SUITE_ONLY_SOCKET_GLOBS=(
    'thurbox-*-e2e'
    'thurbox-e2e-*'
    'thurbox-*-test'
    'thurbox-cm-*'
    'thurbox-leak-*'
    'thurbox-rename-*'
    'thurbox-life-*'
    'thurbox-forget-*'
    'thurbox-stopped-*'
    'thurbox-panic-probe-*'
)

if [ ! -d /proc/self ]; then
    echo "reap-tmux-servers: needs /proc (Linux); nothing done." >&2
    exit 0
fi

is_suite_socket() {
    local name="$1" glob
    for glob in "${SUITE_ONLY_SOCKET_GLOBS[@]}"; do
        # shellcheck disable=SC2053
        [[ $name == $glob ]] && return 0
    done
    return 1
}

socket_of() {
    tr '\0' '\n' < "/proc/$1/cmdline" 2> /dev/null |
        awk '$0 == "-L" { want = 1; next } want { print; exit }'
}

tmpdir_of() {
    local dir
    dir=$(tr '\0' '\n' < "/proc/$1/environ" 2> /dev/null |
        sed -n 's/^TMUX_TMPDIR=//p' | head -1)
    printf '%s' "${dir:-/tmp}"
}

socket_still_usable_by_a_live_harness() { [ -S "$1" ]; }

uid=$(id -u)
found=0
reaped=0

for proc in /proc/[0-9]*; do
    pid=${proc#/proc/}
    # tmux renames the server process; the client keeps its own argv.
    [ "$(cat "$proc/comm" 2> /dev/null)" = "tmux: server" ] || continue
    [ -O "$proc" ] || continue

    socket=$(socket_of "$pid")
    [ -n "$socket" ] || continue
    is_suite_socket "$socket" || continue

    path="$(tmpdir_of "$pid")/tmux-$uid/$socket"
    socket_still_usable_by_a_live_harness "$path" && continue

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
