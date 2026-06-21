#!/usr/bin/env sh
# ui-review capture: drive the REAL thurbox TUI in an isolated sandbox and take a
# PNG screenshot of each major screen/panel using VHS.
#
# This adapts the isolation + seeding model from scripts/demo/record.sh, but emits
# still PNGs (via VHS `Screenshot`) instead of GIF/MP4 video. Nothing here can touch
# your real thurbox sessions, tmux server, or agent accounts:
#
#   * HOME + XDG_{DATA,CONFIG,STATE,CACHE}_HOME point at a throwaway mktemp dir, so
#     agents (if any) boot fresh and the dev DB/config live in the sandbox.
#   * TMUX_TMPDIR points at a throwaway dir, so the `thurbox-dev` tmux server lives
#     in its own socket directory and cleanup can't kill sessions you have running.
#   * The dev binary (0.0.0-dev => dev_build cfg) uses the `thurbox-dev` socket/dirs.
#
# Stub-agent fallback: if none of claude/codex/antigravity/opencode are on PATH, a trivial
# stub agent (a long-lived shell) is registered so the TUI still spawns a session and
# renders every panel.
#
# Usage:  capture.sh [OUTPUT_DIR] [--theme NAME] [--width N] [--height N]
#
#   OUTPUT_DIR  where PNGs + manifest.json go (default: <repo>/target/ui-review/screenshots)
#   --theme     thurbox TUI theme string (default: doom; e.g. "Tokyo Night")
#   --width     VHS viewport width  in px (default 1920; keep >=120 cols equivalent)
#   --height    VHS viewport height in px (default 1080)
#
# Output: logs to stderr; on success prints the screenshots dir to stdout and writes
# <OUTPUT_DIR>/manifest.json. Exit 0 = ok, 1 = recoverable, 2 = fatal preflight error.

set -eu

log()  { printf '[ui-review] %s\n' "$*" >&2; }
die()  { printf '[ui-review] FATAL: %s\n' "$*" >&2; exit 2; }

# --- Resolve skill dir + repo root ------------------------------------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SKILL_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(git -C "$SKILL_DIR" rev-parse --show-toplevel 2>/dev/null) \
    || die "not inside a git repo — run this from the thurbox repo"
cd "$REPO_ROOT"

# --- Args -------------------------------------------------------------------
OUT_DIR=""
DEMO_THEME="${DEMO_THEME:-doom}"
WIDTH=1920
HEIGHT=1080
while [ $# -gt 0 ]; do
    case "$1" in
        --theme)  DEMO_THEME="${2:?--theme needs a value}"; shift 2 ;;
        --width)  WIDTH="${2:?--width needs a value}";       shift 2 ;;
        --height) HEIGHT="${2:?--height needs a value}";     shift 2 ;;
        --*)      die "unknown flag: $1" ;;
        *)        [ -z "$OUT_DIR" ] && OUT_DIR="$1" || die "unexpected arg: $1"; shift ;;
    esac
done
OUT_DIR="${OUT_DIR:-$REPO_ROOT/target/ui-review/screenshots}"
mkdir -p "$OUT_DIR"
# Absolute path (VHS Screenshot paths are literal — no env expansion).
OUT_DIR=$(CDPATH= cd -- "$OUT_DIR" && pwd)

# --- Preflight: required tools ----------------------------------------------
missing=
for tool in cargo git tmux vhs sqlite3; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[ -n "$missing" ] && die "missing required tool(s):$missing (vhs needs ffmpeg + ttyd)"

# --- Build the dev binaries (BEFORE the HOME override so cargo finds ~/.cargo) -
log "Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli >&2
THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
export THURBOX_BIN
[ -x "$THURBOX_BIN" ] || die "dev binary not found at $THURBOX_BIN"

# Map a featured-agent display name to its actual CLI binary. They differ only
# for antigravity, whose binary is `agy` (the Gemini CLI successor); identity for
# everyone else.
agent_command() {
    case "$1" in
        antigravity) echo "agy" ;;
        *) echo "$1" ;;
    esac
}

# --- Which agent CLIs are available? ----------------------------------------
AGENTS=
for a in claude opencode codex antigravity; do
    command -v "$(agent_command "$a")" >/dev/null 2>&1 && AGENTS="$AGENTS $a"
done
STUB=0
if [ -z "$AGENTS" ]; then
    log "no real agent CLI found — using a stub agent so panels still render"
    AGENTS="stub"
    STUB=1
fi

# --- Isolated environment ----------------------------------------------------
SANDBOX=$(mktemp -d "${TMPDIR:-/tmp}/thurbox-ui-review.XXXXXX")
export HOME="$SANDBOX/home"
export XDG_DATA_HOME="$SANDBOX/data"
export XDG_CONFIG_HOME="$SANDBOX/config"
export XDG_STATE_HOME="$SANDBOX/state"
export XDG_CACHE_HOME="$SANDBOX/cache"
export TMUX_TMPDIR="$SANDBOX/tmux"
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
DB_FILE="$XDG_DATA_HOME/thurbox-dev/thurbox.db"
mkdir -p "$HOME" "$CFG_DIR" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
    "$XDG_CACHE_HOME" "$TMUX_TMPDIR"

cleanup() {
    tmux -L thurbox-dev kill-server >/dev/null 2>&1 || true
    rm -rf "$SANDBOX"
}
trap cleanup EXIT INT TERM

# --- Agent registry ----------------------------------------------------------
{
    # shellcheck disable=SC2086 # $AGENTS is a space-separated list, split on purpose
    first=$(printf '%s\n' $AGENTS | head -n1)
    echo "default = \"$first\""
    if [ "$STUB" -eq 1 ]; then
        # A long-lived shell so the tmux pane stays alive and the panel renders.
        printf '\n[[agents]]\nname = "stub"\ncommand = "sh"\nargs = ["-c", "echo ui-review stub agent; exec sh"]\n'
    else
        for a in $AGENTS; do
            printf '\n[[agents]]\nname = "%s"\ncommand = "%s"\n' "$a" "$(agent_command "$a")"
        done
    fi
} > "$CFG_DIR/agents.toml"

# --- Throwaway sample repo (so the file viewer shows a realistic tree) --------
DEMO_REPO="$SANDBOX/sample-project"
mkdir -p "$DEMO_REPO/src" "$DEMO_REPO/tests" "$DEMO_REPO/docs"
cat > "$DEMO_REPO/README.md" <<'EOF'
# sample-project

A tiny demo repository used to exercise the Thurbox UI for review.
EOF
cat > "$DEMO_REPO/src/main.rs" <<'EOF'
fn main() {
    println!("hello from sample-project");
}
EOF
cat > "$DEMO_REPO/src/lib.rs" <<'EOF'
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}
EOF
cat > "$DEMO_REPO/tests/basic.rs" <<'EOF'
#[test]
fn it_adds() {
    assert_eq!(sample_project::add(2, 2), 4);
}
EOF
cat > "$DEMO_REPO/docs/ARCHITECTURE.md" <<'EOF'
# Architecture

Sample document for the file-viewer review.
EOF
git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=ui@thurbox -c user.name=ui add -A
git -C "$DEMO_REPO" -c user.email=ui@thurbox -c user.name=ui commit -q -m "init sample project"

# --- Seed sessions + tasks + an automation ----------------------------------
log "Seeding sessions for:$AGENTS"
for a in $AGENTS; do
    "$CLI_BIN" session create --name "$a" --repo-path "$DEMO_REPO" --agent "$a" >/dev/null
done

log "Seeding tasks + an automation"
"$CLI_BIN" task create --title "Write integration tests" >/dev/null 2>&1 || true
"$CLI_BIN" task create --title "Triage failing CI" --status in_progress >/dev/null 2>&1 || true
"$CLI_BIN" task create --title "Document the search feature" >/dev/null 2>&1 || true
"$CLI_BIN" automation create --name "nightly-triage" --trigger daily \
    --time "09:00" --repo "$DEMO_REPO" \
    --prompt "Triage failing CI and summarize blockers" >/dev/null 2>&1 || true

# Give the agent panes a moment to boot.
sleep 5

# --- Persist the TUI theme so the launched TUI starts in it ------------------
sqlite3 "$DB_FILE" \
    "INSERT INTO metadata (key, value) VALUES ('active_theme', '$DEMO_THEME') \
     ON CONFLICT(key) DO UPDATE SET value = excluded.value" \
    || log "warning: could not set theme (continuing with default)"

# --- Render the tape: substitute placeholders then run VHS -------------------
TAPE_SRC="$SKILL_DIR/tapes/screenshots.tape"
[ -f "$TAPE_SRC" ] || die "tape not found: $TAPE_SRC"
TAPE_RUN="$SANDBOX/screenshots.tape"
# __SHOT_DIR__, __WIDTH__, __HEIGHT__ are literal placeholders in the tape.
sed -e "s|__SHOT_DIR__|$OUT_DIR|g" \
    -e "s|__WIDTH__|$WIDTH|g" \
    -e "s|__HEIGHT__|$HEIGHT|g" \
    "$TAPE_SRC" > "$TAPE_RUN"

log "Running VHS (theme: $DEMO_THEME, ${WIDTH}x${HEIGHT}) -> $OUT_DIR"
vhs "$TAPE_RUN" >&2 || log "warning: VHS exited non-zero; capturing whatever PNGs exist"

# --- Manifest ---------------------------------------------------------------
# One entry per screenshot the tape is expected to produce. Only list files that
# actually exist so the analyze phase never references a missing PNG.
emit() { # file label screen keys
    [ -f "$OUT_DIR/$1" ] || return 0
    printf '  {"file":"%s","label":"%s","screen":"%s","keys":"%s"}' "$1" "$2" "$3" "$4"
}
{
    echo "["
    first=1
    while IFS='|' read -r f l s k; do
        [ -z "$f" ] && continue
        entry=$(emit "$f" "$l" "$s" "$k") || true
        [ -z "$entry" ] && continue
        [ "$first" -eq 1 ] || printf ',\n'
        first=0
        printf '%s' "$entry"
    done <<'MANIFEST'
01-session-list.png|Session list + terminal (default view)|session-list|launch
02-second-session.png|A different session selected|session-list|Ctrl+J
03-info-panel.png|Session info panel|info-panel|Ctrl+B
04-file-viewer.png|File viewer (repo tree)|file-viewer|Ctrl+E
05-tasks-panel.png|Tasks panel (todo list)|tasks|Ctrl+W
06-automations.png|Automations pane|automations|Ctrl+P
07-global-search.png|Global search strip|search|Ctrl+A + query
08-theme-picker.png|Theme picker|theme|Ctrl+Y
09-repo-picker.png|New-session / repo picker|new-session|Ctrl+N
10-keybindings.png|Keybindings help + editor|keybindings|Ctrl+G
MANIFEST
    printf '\n]\n'
} > "$OUT_DIR/manifest.json"

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' 2>/dev/null | wc -l | tr -d ' ')
log "Captured $count screenshot(s)."
[ "$count" -eq 0 ] && { log "no screenshots produced"; exit 1; }

# stdout: the directory the report phase reads from.
printf '%s\n' "$OUT_DIR"
