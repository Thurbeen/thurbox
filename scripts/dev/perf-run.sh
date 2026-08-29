#!/usr/bin/env bash
#
# Run the real thurbox under a reproducible load and report what it cost.
#
# `benches/frame_cost.rs` measures the pieces of a frame in isolation; this runs
# the whole binary — real tmux panes, a real vt100 grid per session, the real
# render loop — and reports the two numbers a user actually feels: **CPU while
# an agent prints**, and the frame/republish/tick percentiles the loop logged.
#
# The docs' only steady-state instruction was "launch it and leave it idle",
# which measures the one regime nobody complains about. This is the other one.
#
#   scripts/dev/perf-run.sh                    # 8 sessions, 1 printing, 30s
#   scripts/dev/perf-run.sh -n 20 -d 60        # 20 sessions, 60s
#   scripts/dev/perf-run.sh -n 20 -p 3         # 3 of them printing
#   scripts/dev/perf-run.sh --idle             # nothing printing, for the floor
#   scripts/dev/perf-run.sh --json             # one machine-readable line
#
# Fully isolated: a private HOME, XDG root and TMUX_TMPDIR (so the cleanup
# `kill-server` can never reach a real server), the sandbox helper every other
# dev script uses. The agent is `sh` printing on a timer, so the measurement is
# of thurbox and not of whichever coding CLI happened to be installed.
#
# thurbox needs a terminal, and it must be a terminal of a KNOWN SIZE — a frame
# costs what its cells cost, so a run at whatever the invoking window happens to
# be is not comparable with the last one. So it runs inside an outer tmux
# session created at an exact size, on the same private socket.

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
# Whether the run carries THURBOX_PERF_LOG. On, the loop keeps histograms and
# republishes a JSON snapshot to SQLite every few seconds -- which is itself
# work, and work a default run does not do. CPU here is measured from
# /proc, so the log can be turned off and the headline number still stands;
# only the percentile lines go away.
PERF_LOG=1
PROFILE=release
# Lines a printing agent emits per second. 30 is ADR-P17's workload, so a number
# from this script is comparable with the table in that ADR.
RATE=30
# One URL every N printed lines (0 = none). A knob because the link scan is
# keyed on the surface's output stamp and re-runs per painted frame while output
# arrives -- so "what do links cost while an agent prints" is a question the
# harness should be able to answer, not one to reason about.
URL_EVERY=12
# How many sessions report themselves `working`. The animation clock advances
# eight times a second while any session does, and it is part of the pure-pane
# cache key -- so every pane re-renders at that rate for a spinner glyph. `sh`
# runs no status hook, so without this the harness measures that cost as zero
# while a real profile pays it nearly all the time.
WORKING=0

usage() {
    cat >&2 <<'EOF'
usage: perf-run.sh [options]
  -n N        sessions to create (default 8)
  -p N        how many of them print (default 1; --idle sets 0)
  -d SECS     how long to measure (default 30)
  -s COLSxROWS  terminal size (default 200x50)
  -r N        lines per second each printing agent emits (default 30)
  -u N        a URL every N printed lines (default 12; 0 = never)
  -w N        mark N sessions `working`, so the animation clock runs (default 0)
  --idle      nothing prints — measures the settled floor
  --debug     measure the dev profile instead of release
  --no-perf-log  run without THURBOX_PERF_LOG (CPU only, no percentiles) --
                 the control for "is the instrumentation the cost?"
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
        --idle) PRINTING=0; shift ;;
        --debug) PROFILE=dev; shift ;;
        --no-perf-log) PERF_LOG=0; shift ;;
        --json) JSON=1; shift ;;
        -h|--help) usage ;;
        *) echo "perf-run.sh: unknown option '$1'" >&2; usage ;;
    esac
done

say() { [ "$JSON" = "1" ] || echo "$@" >&2; }

# --- build ------------------------------------------------------------------
#
# Release by default. A dev build runs the interpreter at opt-level 1 and its
# numbers are not the ones a user sees; `--debug` is for attributing a change
# quickly, never for a figure worth writing down.

if [ "$PROFILE" = release ]; then
    BIN_DIR="$REPO_ROOT/target/release"
    say "building (release)…"
    cargo build --release --bin thurbox --bin thurbox-cli >/dev/null 2>&1
else
    BIN_DIR="$REPO_ROOT/target/debug"
    say "building (dev)…"
    cargo build --bin thurbox --bin thurbox-cli >/dev/null 2>&1
fi

# --- an isolated world ------------------------------------------------------

tbx_sandbox_init_full fresh
# The helper puts target/debug first for the agent-hook case; this measures a
# chosen profile, so the chosen one wins.
PATH="$BIN_DIR:$PATH"
export PATH
trap 'tbx_sandbox_teardown' EXIT

# The agent. `sh` rather than a coding CLI: this measures thurbox's cost of
# *carrying* output, and a real agent would add its own — plus its rate would be
# whatever the model felt like, which is not a controlled variable.
AGENT_DIR="$TBX_SANDBOX_ROOT/agent"
mkdir -p "$AGENT_DIR"
cat > "$AGENT_DIR/noisy" <<EOF
#!/bin/sh
# One printing agent: \$RATE lines a second of plausible agent output, forever.
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
cat > "$AGENT_DIR/quiet" <<'EOF'
#!/bin/sh
# A session that exists, is attached, and says nothing.
while :; do sleep 3600; done
EOF
chmod +x "$AGENT_DIR/noisy" "$AGENT_DIR/quiet"

mkdir -p "$XDG_CONFIG_HOME/thurbox-dev"
cat > "$XDG_CONFIG_HOME/thurbox-dev/agents.toml" <<EOF
default = "quiet"

[[agents]]
name = "quiet"
command = "$AGENT_DIR/quiet"

[[agents]]
name = "noisy"
command = "$AGENT_DIR/noisy"
EOF

# A repository for the sessions to live in. Bare and local: worktree creation is
# a startup cost, not a steady-state one, and a real repo would make each run
# depend on whatever is checked out.
REPO="$TBX_SANDBOX_ROOT/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" -c user.email=perf@example.com -c user.name=perf commit -q \
    --allow-empty -m "root"

# --- run it -----------------------------------------------------------------
#
# In an outer tmux window of an exact size: a frame costs what its cells cost,
# so a run at whatever the invoking window happens to be is not comparable with
# the last one.
#
# The TUI starts on the EMPTY database, before any session exists. The v1->v2
# consent gate fires for a profile with session history and no acknowledgment,
# and it waits for a keypress -- so seeding first left the binary sitting on the
# gate for the whole run, reporting a very restful 0% of a core. Sessions are
# created underneath it instead and adopted through the ordinary `data_version`
# poll, which is also closer to what a real profile does.

LOG_DIR="$XDG_DATA_HOME/thurbox-dev"
say "starting thurbox at ${COLS}x${ROWS}…"
LAUNCH="'$BIN_DIR/thurbox'"
[ "$PERF_LOG" = "1" ] && LAUNCH="THURBOX_PERF_LOG=1 $LAUNCH"
tmux -L "$TBX_DEV_SOCKET" new-session -d -s perf-harness -x "$COLS" -y "$ROWS" \
    "$LAUNCH"
sleep 3

# THE process, not A process. Two traps here, and both report a number that
# looks entirely plausible:
#
#   * `pgrep -f "$BIN_DIR/thurbox"` also matches `$BIN_DIR/thurbox-cli`, whose
#     path has it as a prefix;
#   * `pgrep -x thurbox` matches the developer's OWN running thurbox, which on
#     this machine is the likeliest process of that name. Every measurement then
#     reports their real instance's CPU -- the same ~17% for an idle harness, a
#     printing one, one session or twenty, because the harness was never the
#     thing being measured.
#
# So the sandbox is what identifies it: only this run's thurbox has this run's
# private XDG_DATA_HOME in its environment.
PID=""
for candidate in $(pgrep -x thurbox 2>/dev/null || true); do
    if tr '\0' '\n' < "/proc/$candidate/environ" 2>/dev/null |
        grep -qxF "XDG_DATA_HOME=$XDG_DATA_HOME"; then
        PID="$candidate"
        break
    fi
done
if [ -z "$PID" ]; then
    echo "perf-run.sh: thurbox did not start. Last log lines:" >&2
    tail -20 "$LOG_DIR"/thurbox.log* 2>/dev/null >&2 || true
    exit 1
fi

say "creating $SESSIONS sessions ($PRINTING printing)…"
for i in $(seq 1 "$SESSIONS"); do
    agent=quiet
    [ "$i" -le "$PRINTING" ] && agent=noisy
    thurbox-cli session create \
        --name "perf-session-$i" \
        --repo-path "$REPO" \
        --agent "$agent" >/dev/null
done

# A status hook's write, without a hook. `session signal` takes its identity
# from the injected `THURBOX_SESSION`, so setting it here is exactly what an
# agent's own hook does from inside its pane.
if [ "$WORKING" -gt 0 ]; then
    say "marking $WORKING sessions working…"
    thurbox-cli session list --json |
        tr ',' '\n' | grep -o '"id":"[^"]*"' | cut -d'"' -f4 | head -n "$WORKING" |
        while read -r id; do
            THURBOX_SESSION="$id" thurbox-cli session signal --state working >/dev/null
        done
fi

# Settle: adopting a pane, taking the first snapshot and painting the first
# frame are startup, not steady state. Measuring across them would report the
# startup once as if it happened every second.
sleep 8
if ! kill -0 "$PID" 2>/dev/null; then
    echo "perf-run.sh: thurbox exited during the settle. Last log lines:" >&2
    tail -20 "$LOG_DIR"/thurbox.log* 2>/dev/null >&2 || true
    exit 1
fi

jiffies() { awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || echo 0; }
# Every thread, so a cost moved onto a worker is still counted. Moving work off
# the render thread is the right fix for a stall and does nothing for a laptop
# battery, and only the total tells the two apart.
tree_jiffies() {
    total=0
    for t in "/proc/$1/task"/*; do
        [ -r "$t/stat" ] || continue
        total=$((total + $(awk '{print $14 + $15}' "$t/stat")))
    done
    echo "$total"
}

BEFORE_MAIN="$(jiffies "$PID")"
BEFORE_ALL="$(tree_jiffies "$PID")"
sleep "$DURATION"
AFTER_MAIN="$(jiffies "$PID")"
AFTER_ALL="$(tree_jiffies "$PID")"

HZ="$(getconf CLK_TCK)"
main_pct="$(awk "BEGIN { printf \"%.2f\", ($AFTER_MAIN - $BEFORE_MAIN) * 100 / $HZ / $DURATION }")"
all_pct="$(awk "BEGIN { printf \"%.2f\", ($AFTER_ALL - $BEFORE_ALL) * 100 / $HZ / $DURATION }")"

# The loop's own view, from the last window it logged. `perf_window` is emitted
# every PERF_WINDOW_TICKS iterations, so the last complete one is the steady
# state; earlier ones can still contain the startup.
WINDOW="$(grep -h perf_window "$LOG_DIR"/thurbox.log* 2>/dev/null | tail -1 || true)"
SNAPSHOT="$(thurbox-cli perf 2>/dev/null || true)"

# The sandbox is `fresh`, so teardown takes the log with it. Copy it out first:
# a `perf_window` line is a summary, and the question after reading one is always
# "what else did it say" -- a slow op, a warning, a panic on a reader thread.
KEPT_LOG="$REPO_ROOT/target/perf-run.log"
mkdir -p "$REPO_ROOT/target"
cat "$LOG_DIR"/thurbox.log* > "$KEPT_LOG" 2>/dev/null || true

tmux -L "$TBX_DEV_SOCKET" kill-session -t perf-harness >/dev/null 2>&1 || true

field() { echo "$WINDOW" | grep -o "$1=[0-9]*" | head -1 | cut -d= -f2; }

if [ "$JSON" = "1" ]; then
    printf '{"sessions":%s,"printing":%s,"size":"%sx%s","seconds":%s,' \
        "$SESSIONS" "$PRINTING" "$COLS" "$ROWS" "$DURATION"
    printf '"cpu_render_thread_pct":%s,"cpu_all_threads_pct":%s,' \
        "$main_pct" "$all_pct"
    printf '"frame_p50_us":%s,"frame_p95_us":%s,"republish_p50_us":%s,"tick_p50_us":%s,' \
        "$(field frame_p50_us || echo 0)" "$(field frame_p95_us || echo 0)" \
        "$(field republish_p50_us || echo 0)" "$(field tick_p50_us || echo 0)"
    printf '"frames":%s,"iterations":%s}\n' \
        "$(field frames || echo 0)" "$(field iterations || echo 0)"
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
