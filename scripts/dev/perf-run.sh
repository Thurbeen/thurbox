#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"

SESSIONS=8
PRINTING=1
DURATION=30
COLS=200
ROWS=50
JSON=0
PERF_LOG=1
PROFILE=release
ADR_P17_LINES_PER_SECOND=30
RATE=$ADR_P17_LINES_PER_SECOND
URL_EVERY=12
WORKING=0
WORKING_TICK_SECS_UNDER_10S_QUIESCENCE=4
STARTUP_SETTLE_SECS=8
BACKFILL=0
SCROLLBACK=""
SEARCH=""
TYPING=0
FOCUS_LAST=0
PREBUILT=""

usage() {
    cat >&2 <<'EOF'
usage: perf-run.sh [options]
  -n N        sessions to create (default 8)
  -p N        how many of them print (default 1; --idle sets 0)
  -d SECS     how long to measure (default 30)
  -s COLSxROWS  terminal size (default 200x50)
  -r N        lines per second each printing agent emits (default 30)
  -u N        a URL every N printed lines (default 12; 0 = never)
  -w N        mark N sessions `working`, so the animation clock runs (default 0);
              a non-printing one animates a progress line, as a real agent does
  --focus-last  select the last session (quiet when -p is smaller than -n)
  --idle      nothing prints — measures the settled floor
  --debug     measure the dev profile instead of release
  --no-perf-log  run without THURBOX_PERF_LOG (CPU only, no percentiles) --
                 the control for "is the instrumentation the cost?"
  -b N        every agent prints N lines as it starts, to fill its scrollback
  --scrollback N  set scrollback_lines for the run
  --search Q  open global search and type Q before measuring
  --typing    with --search, keep erasing and retyping Q while measuring
  --bin-dir D measure the thurbox, thurbox-cli, fleet and runtime-mux in D
              instead of building
  --json      one machine-readable line instead of the report
EOF
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        -n) SESSIONS="$2"; shift 2 ;;
        -p) PRINTING="$2"; shift 2 ;;
        -d) DURATION="$2"; shift 2 ;;
        -r) RATE="$2"; shift 2 ;;
        -u) URL_EVERY="$2"; shift 2 ;;
        -w) WORKING="$2"; shift 2 ;;
        -s) COLS="${2%x*}"; ROWS="${2#*x}"; shift 2 ;;
        -b) BACKFILL="$2"; shift 2 ;;
        --scrollback) SCROLLBACK="$2"; shift 2 ;;
        --search) SEARCH="$2"; shift 2 ;;
        --typing) TYPING=1; shift ;;
        --bin-dir) PREBUILT="$2"; shift 2 ;;
        --idle) PRINTING=0; shift ;;
        --focus-last) FOCUS_LAST=1; shift ;;
        --debug) PROFILE=dev; shift ;;
        --no-perf-log) PERF_LOG=0; shift ;;
        --json) JSON=1; shift ;;
        -h|--help) usage ;;
        *) echo "perf-run.sh: unknown option '$1'" >&2; usage ;;
    esac
done

say() { [ "$JSON" = "1" ] || echo "$@" >&2; }

if [ -n "${THURBOX_GATE:-}" ] && [ -z "${THURBOX_PERF_ALLOW_IN_GATE:-}" ]; then
    cat >&2 <<'REFUSE'
perf-run.sh: refusing to run inside a validation step.

This is a benchmark, not a test: it builds a release binary, runs agents for
tens of seconds and measures CPU. Its reading is only meaningful on a quiet
machine, and a gate is the opposite of one.

Nothing here needs it to pass. The deterministic coverage is ordinary tests --
`cargo nextest run --all` -- and the perf claims are asserted on counters and
change-signals in tests/kernel_frame_cost.rs and tests/kernel_perf.rs, never on
a clock (docs/thurbox/performance.md, ADR-P5).

To take a reading by hand, on a machine you are not otherwise using:
    just perf -n 19 -p 3 -s 255x62
    just perf -n 19 -p 3 -s 255x62 -u 0     # the paired control

Set THURBOX_PERF_ALLOW_IN_GATE=1 to override this, if you really mean to.
REFUSE
    exit 2
fi

if [ -n "$PREBUILT" ]; then
    BIN_DIR="$(cd "$PREBUILT" && pwd)"
    say "measuring the binaries in $BIN_DIR…"
elif [ "$PROFILE" = release ]; then
    BIN_DIR="$REPO_ROOT/target/release"
    say "building (release)…"
    cargo build --release --bin thurbox --bin thurbox-cli --bin fleet \
        --bin runtime-mux --bin workspace-git >/dev/null 2>&1
else
    BIN_DIR="$REPO_ROOT/target/debug"
    say "building (dev)…"
    cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux --bin workspace-git \
        >/dev/null 2>&1
fi

put_measured_binaries_ahead_of_sandbox_debug_ones() {
    PATH="$BIN_DIR:$PATH"
    export PATH
}

tbx_sandbox_init_full fresh
put_measured_binaries_ahead_of_sandbox_debug_ones
tbx_sandbox_fleet
trap 'tbx_sandbox_teardown' EXIT

AGENT_DIR="$TBX_SANDBOX_ROOT/agent"
mkdir -p "$AGENT_DIR"
BACKFILL_CMD=":"
if [ "$BACKFILL" -gt 0 ]; then
    BACKFILL_CMD="awk 'BEGIN { split(\"the session worktree branch failed compile error src/main.rs fn let tests passed running cargo warning: unused variable match diff\", w, \" \"); for (i = 1; i <= $BACKFILL; i++) { line = \"\"; for (j = 0; j < 9; j++) line = line \" \" w[(i * 7 + j * 13) % 19 + 1]; print i line } }'"
fi
cat > "$AGENT_DIR/noisy" <<EOF
#!/bin/sh
# One printing agent: \$RATE lines a second of plausible agent output, forever.
$BACKFILL_CMD
n=0
while :; do
    n=\$((n + 1))
    printf '  %4d | rewrote src/kernel/host/publish.rs and re-ran the suite\n' "\$n"
    if [ $URL_EVERY -gt 0 ] && [ \$((n % $URL_EVERY)) -eq 0 ]; then
        printf '  see https://github.com/Thurbeen/thurbox/pull/%d for the rest\n' "\$n"
    fi
    sleep $(awk "BEGIN { printf \"%.4f\", 1 / $RATE }")
done
EOF
cat > "$AGENT_DIR/quiet" <<EOF
#!/bin/sh
# A session that exists, is attached, and says nothing.
$BACKFILL_CMD
while :; do sleep 3600; done
EOF
cat > "$AGENT_DIR/ticking" <<EOF
#!/bin/sh
# A session that reports itself \`working\` and animates a progress line, which
# is what every real agent does while a turn runs -- and what keeps thurbox's
# output-quiescence fallback from folding the state back to idle. The state is
# reported the way an agent's hook reports it: Fleet gave the session
# FLEET_SIGNAL_FILE.
fleet signal working 2>/dev/null || true
$BACKFILL_CMD
n=0
while :; do
    n=\$((n + 1))
    printf '  (%ds - esc to interrupt)\n' "\$n"
    sleep $WORKING_TICK_SECS_UNDER_10S_QUIESCENCE
done
EOF
chmod +x "$AGENT_DIR/noisy" "$AGENT_DIR/quiet" "$AGENT_DIR/ticking"

mkdir -p "$XDG_CONFIG_HOME/thurbox-dev"
if [ -n "$SCROLLBACK" ]; then
    printf 'config_version = 1\nscrollback_lines = %s\n' "$SCROLLBACK" \
        > "$XDG_CONFIG_HOME/thurbox-dev/settings.toml"
fi

REPO="$TBX_SANDBOX_ROOT/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" -c user.email=perf@example.com -c user.name=perf commit -q \
    --allow-empty -m "root"

tbx_sandbox_fleet_up
LOG_DIR="$XDG_DATA_HOME/thurbox-dev"
say "starting thurbox at ${COLS}x${ROWS}…"
LAUNCH="'$BIN_DIR/thurbox'"
[ "$PERF_LOG" = "1" ] && LAUNCH="THURBOX_PERF_LOG=1 $LAUNCH"
tmux -L "$TBX_DEV_SOCKET" new-session -d -s perf-harness -x "$COLS" -y "$ROWS" \
    "$LAUNCH"
sleep 3

thurbox_pid_by_this_sandbox_not_by_name() {
    for candidate in $(pgrep -x thurbox 2>/dev/null || true); do
        if tr '\0' '\n' < "/proc/$candidate/environ" 2>/dev/null |
            grep -qxF "XDG_DATA_HOME=$XDG_DATA_HOME"; then
            echo "$candidate"
            return
        fi
    done
}

PID="$(thurbox_pid_by_this_sandbox_not_by_name)"
if [ -z "$PID" ]; then
    echo "perf-run.sh: thurbox did not start. Last log lines:" >&2
    tail -20 "$LOG_DIR"/thurbox.log* 2>/dev/null >&2 || true
    exit 1
fi

say "creating $SESSIONS sessions ($PRINTING printing, $WORKING working)…"
for i in $(seq 1 "$SESSIONS"); do
    if [ "$i" -le "$PRINTING" ]; then
        agent=noisy
    elif [ "$i" -le "$WORKING" ]; then
        agent=ticking
    else
        agent=quiet
    fi
    tbx_fleet_session "perf-session-$i" "$REPO" "$AGENT_DIR/$agent"
done

sleep "$STARTUP_SETTLE_SECS"

if [ "$FOCUS_LAST" = "1" ]; then
    thurbox-cli ui action session.focus --session "perf-session-$SESSIONS" >/dev/null
    sleep 2
fi
if ! kill -0 "$PID" 2>/dev/null; then
    echo "perf-run.sh: thurbox exited during the settle. Last log lines:" >&2
    tail -20 "$LOG_DIR"/thurbox.log* 2>/dev/null >&2 || true
    exit 1
fi

# `C-_` is what tmux sends for ctrl+/, and the kernel folds it into that chord.
TYPER=""
if [ -n "$SEARCH" ]; then
    tmux -L "$TBX_DEV_SOCKET" send-keys -t perf-harness C-_
    sleep 1
    tmux -L "$TBX_DEV_SOCKET" send-keys -t perf-harness -l "$SEARCH"
    sleep 3
    if [ "$TYPING" = "1" ]; then
        (
            while :; do
                i=0
                while [ "$i" -lt "${#SEARCH}" ]; do
                    tmux -L "$TBX_DEV_SOCKET" send-keys -t perf-harness BSpace
                    sleep 0.12
                    i=$((i + 1))
                done
                i=1
                while [ "$i" -le "${#SEARCH}" ]; do
                    tmux -L "$TBX_DEV_SOCKET" send-keys -t perf-harness -l \
                        "$(printf '%s' "$SEARCH" | cut -c"$i")"
                    sleep 0.12
                    i=$((i + 1))
                done
                sleep 0.5
            done
        ) &
        TYPER=$!
    fi
fi

jiffies() { awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || echo 0; }
all_threads_jiffies() {
    total=0
    for t in "/proc/$1/task"/*; do
        [ -r "$t/stat" ] || continue
        total=$((total + $(awk '{print $14 + $15}' "$t/stat")))
    done
    echo "$total"
}

BEFORE_MAIN="$(jiffies "$PID")"
BEFORE_ALL="$(all_threads_jiffies "$PID")"
sleep "$DURATION"
AFTER_MAIN="$(jiffies "$PID")"
AFTER_ALL="$(all_threads_jiffies "$PID")"
[ -n "$TYPER" ] && kill "$TYPER" 2>/dev/null

HZ="$(getconf CLK_TCK)"
main_pct="$(awk "BEGIN { printf \"%.2f\", ($AFTER_MAIN - $BEFORE_MAIN) * 100 / $HZ / $DURATION }")"
all_pct="$(awk "BEGIN { printf \"%.2f\", ($AFTER_ALL - $BEFORE_ALL) * 100 / $HZ / $DURATION }")"

WINDOW="$(grep -h perf_window "$LOG_DIR"/thurbox.log* 2>/dev/null | tail -1 || true)"
SNAPSHOT="$(thurbox-cli perf 2>/dev/null || true)"

KEPT_LOG="$REPO_ROOT/target/perf-run.log"
mkdir -p "$REPO_ROOT/target"
cat "$LOG_DIR"/thurbox.log* > "$KEPT_LOG" 2>/dev/null || true

tmux -L "$TBX_DEV_SOCKET" kill-session -t perf-harness >/dev/null 2>&1 || true

field() {
    value="$(echo "$WINDOW" | grep -o "$1=[0-9]*" | head -1 | cut -d= -f2)"
    echo "${value:-0}"
}

if [ "$JSON" = "1" ]; then
    printf '{"sessions":%s,"printing":%s,"working":%s,"rate":%s,"url_every":%s,' \
        "$SESSIONS" "$PRINTING" "$WORKING" "$RATE" "$URL_EVERY"
    printf '"size":"%sx%s","seconds":%s,' "$COLS" "$ROWS" "$DURATION"
    printf '"cpu_render_thread_pct":%s,"cpu_all_threads_pct":%s,' \
        "$main_pct" "$all_pct"
    printf '"frame_p50_us":%s,"frame_p95_us":%s,"republish_p50_us":%s,"tick_p50_us":%s,' \
        "$(field frame_p50_us)" "$(field frame_p95_us)" \
        "$(field republish_p50_us)" "$(field tick_p50_us)"
    search_json="$(printf '%s' "$SEARCH" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    printf '"frame_max_us":%s,"republish_p95_us":%s,"search":"%s","typing":%s,' \
        "$(field frame_max_us)" "$(field republish_p95_us)" "$search_json" "$TYPING"
    printf '"frames":%s,"iterations":%s}\n' \
        "$(field frames)" "$(field iterations)"
    exit 0
fi

cat <<EOF

thurbox under load — $SESSIONS sessions, $PRINTING printing at ${RATE}/s (url every ${URL_EVERY}, ${WORKING} working), ${COLS}x${ROWS}, ${DURATION}s

  CPU, render thread    ${main_pct}% of a core
  CPU, whole process    ${all_pct}% of a core

EOF
if [ -n "$WINDOW" ]; then
    echo "  last perf_window:"
    echo "    ${WINDOW}"
else
    echo "  (no perf_window line — the run was too short to fill one)"
fi
[ -n "$SNAPSHOT" ] && { echo; echo "  perf snapshot:"; echo "    $SNAPSHOT"; }
echo
echo "  full log kept at target/perf-run.log"
echo
