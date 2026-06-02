#!/usr/bin/env sh
# Regenerate ALL Thurbox demo media in one pass, using REAL coding-agent CLIs.
#
# This single script records every video pair under docs/media/:
#
#   * thurbox-demo.{gif,mp4}            (agents.tape          — the hero demo)
#   * thurbox-file-manager.{gif,mp4}    (file-manager.tape)
#   * thurbox-info-panel.{gif,mp4}      (info-panel.tape)
#   * thurbox-theme.{gif,mp4}           (theme.tape)
#   * thurbox-session-creation.{gif,mp4}(session-creation.tape)
#   * automations-demo.{gif,mp4}        (automations.tape)
#   * tasks-demo.{gif,mp4}              (tasks.tape)
#   * search-demo.{gif,mp4}             (search.tape)
#
# Every clip drives the actual `claude`, `opencode`, `codex` and `gemini` CLIs —
# one per thurbox session — to showcase real multi-agent orchestration. No prompt
# is sent to any agent; they are launched and left on their start screens.
#
# Isolation (so this never touches your real thurbox, tmux, or agent accounts):
#   * HOME points at a throwaway dir  -> agents boot FRESH (no account/email or
#     past conversations leak into the video). Some CLIs may show a login/welcome
#     screen rather than a chat UI; that is expected for a clean-room recording.
#   * TMUX_TMPDIR points at a throwaway dir -> the `thurbox-dev` tmux server lives
#     in its own socket directory, so cleanup can't kill dev sessions you already
#     have running.
#   * XDG_{DATA,CONFIG,STATE,CACHE}_HOME point at a throwaway dir.
#
# Requirements: cargo, git, tmux, vhs (+ ffmpeg + ttyd) and whichever agent CLIs
# you want to feature (claude / opencode / codex / gemini). Missing agents are
# skipped with a warning.
#
# Usage:  scripts/demo/record.sh [tape-stem ...]
#
#   With no args, records every tape below. Pass one or more tape stems to
#   re-record only a subset, e.g. `record.sh theme automations`.

set -eu

# Tapes to record (stems of scripts/demo/<stem>.tape), hero first. `agents` is
# the combined hero demo (docs/media/thurbox-demo.*); the rest are per-feature
# clips (`automations` -> automations-demo.*, `tasks` -> tasks-demo.*, `search`
# -> search-demo.*, others -> thurbox-<stem>.*).
ALL_TAPES="agents file-manager info-panel theme session-creation automations tasks search"
TAPES="${*:-$ALL_TAPES}"

# thurbox TUI theme every clip starts in (persisted string in metadata.active_theme,
# see src/session/theme_config.rs). The `theme` clip switches away from it to show
# the picker, so we re-apply this before EVERY tape to keep all videos on-brand.
DEMO_THEME="${DEMO_THEME:-doom}"

# --- Locate the repo root (this script lives in scripts/demo/) ---------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
cd "$REPO_ROOT"

# Validate requested tapes exist before doing any expensive setup.
for tape in $TAPES; do
    if [ ! -f "$SCRIPT_DIR/$tape.tape" ]; then
        echo "error: no such tape: $SCRIPT_DIR/$tape.tape" >&2
        echo "  available: $ALL_TAPES" >&2
        exit 1
    fi
done

# --- Preflight: required tools ----------------------------------------------
missing=
for tool in cargo git tmux vhs sqlite3; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "error: missing required tool(s):$missing" >&2
    echo "  vhs:  https://github.com/charmbracelet/vhs (needs ffmpeg + ttyd)" >&2
    exit 1
fi

# Which agent CLIs are available? Feature only the ones present.
AGENTS=
for a in claude opencode codex gemini; do
    if command -v "$a" >/dev/null 2>&1; then
        AGENTS="$AGENTS $a"
    else
        echo "warning: '$a' not found on PATH — skipping it in the demo" >&2
    fi
done
if [ -z "$AGENTS" ]; then
    echo "error: none of claude/opencode/codex/gemini are installed" >&2
    exit 1
fi

# --- Build the dev binaries (version 0.0.0-dev => dev_build cfg) -------------
# Build BEFORE the HOME override so cargo still finds ~/.cargo.
echo "==> Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli

THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
export THURBOX_BIN   # consumed by the tapes (they `exec "$THURBOX_BIN"`)

# --- Isolated environment ----------------------------------------------------
DEMO_HOME=$(mktemp -d "${TMPDIR:-/tmp}/thurbox-demo.XXXXXX")
export HOME="$DEMO_HOME/home"            # fresh agent auth (no real creds/history)
export XDG_DATA_HOME="$DEMO_HOME/data"
export XDG_CONFIG_HOME="$DEMO_HOME/config"
export XDG_STATE_HOME="$DEMO_HOME/state"
export XDG_CACHE_HOME="$DEMO_HOME/cache"
export TMUX_TMPDIR="$DEMO_HOME/tmux"     # isolate the tmux socket DIRECTORY
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"   # dev_build subdir
DB_FILE="$XDG_DATA_HOME/thurbox-dev/thurbox.db"  # SQLite db (dev_build subdir)
mkdir -p "$HOME" "$CFG_DIR" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
    "$XDG_CACHE_HOME" "$TMUX_TMPDIR"

cleanup() {
    # The isolated tmux server (in TMUX_TMPDIR) hosts every agent pane, so this
    # single kill reaps all the real agent processes too — and cannot reach any
    # tmux server outside this throwaway directory.
    tmux -L thurbox-dev kill-server >/dev/null 2>&1 || true
    rm -rf "$DEMO_HOME"
}
trap cleanup EXIT INT TERM

# --- Agent registry: one entry per available CLI, launched with no args ------
{
    first=$(printf '%s\n' $AGENTS | head -n1)
    echo "default = \"$first\""
    for a in $AGENTS; do
        printf '\n[[agents]]\nname = "%s"\ncommand = "%s"\n' "$a" "$a"
    done
} > "$CFG_DIR/agents.toml"

# --- A throwaway sample repo so the file viewer shows a realistic tree --------
DEMO_REPO="$DEMO_HOME/sample-project"
mkdir -p "$DEMO_REPO/src" "$DEMO_REPO/tests" "$DEMO_REPO/docs"
cat > "$DEMO_REPO/README.md" <<'EOF'
# sample-project

A tiny demo repository used to showcase the Thurbox file viewer.
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

Sample document for the file-viewer demo.
EOF
git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo add -A
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo \
    commit -q -m "init sample project"

# --- Pre-seed one session per agent so the TUI opens populated ---------------
echo "==> Seeding one session per agent:$AGENTS"
for a in $AGENTS; do
    "$CLI_BIN" session create --name "$a" --repo-path "$DEMO_REPO" --agent "$a" >/dev/null
done

# --- Pre-seed a few tasks + an automation -----------------------------------
# These give the `tasks` and `search` clips real content to render (the search
# strip searches across sessions, tasks AND automations at once). Only needed
# for those two tapes, but seeding is cheap and harmless for the others.
if printf '%s ' $TAPES | grep -Eq '(^| )(tasks|search)( |$)'; then
    echo "==> Seeding demo tasks + an automation"
    # A plain local todo plus one already in progress, so the checkbox glyphs
    # (todo/in-progress/done) all show in the list.
    "$CLI_BIN" task create --title "Write integration tests" >/dev/null 2>&1 || true
    "$CLI_BIN" task create --title "Triage failing CI" --status in_progress \
        >/dev/null 2>&1 || true
    "$CLI_BIN" task create --title "Document the search feature" >/dev/null 2>&1 || true
    # An automation (spawn action, inferred from --repo) so the search demo has
    # a matching automation result too.
    "$CLI_BIN" automation create --name "nightly-triage" --trigger daily \
        --time "09:00" --repo "$DEMO_REPO" \
        --prompt "Triage failing CI and summarize blockers" >/dev/null 2>&1 || true
fi

# Give the real CLIs a moment to boot before VHS starts capturing. Each tape
# relaunches the TUI against the same seeded sessions, so they only need to be
# warm once.
sleep 6

# --- Record -----------------------------------------------------------------
# Each tape declares its own Output paths, so one VHS run == one output pair.
# Loop over the requested tapes, rendering each into docs/media/.
# Persist the TUI theme into the seeded db so the next launched TUI starts in it.
# The db + metadata table already exist (the `session create` calls above opened
# them). No TUI is running between vhs invocations, so this write is conflict-free.
set_theme() {
    sqlite3 "$DB_FILE" \
        "INSERT INTO metadata (key, value) VALUES ('active_theme', '$1') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value"
}

for tape in $TAPES; do
    # Re-apply before every tape: the `theme` clip switches themes (and persists
    # the change), so without this any tape after it would start on the wrong one.
    set_theme "$DEMO_THEME"
    echo "==> Recording $tape.tape (theme: $DEMO_THEME) ..."
    vhs "$SCRIPT_DIR/$tape.tape"
done

echo "==> Done. Updated docs/media/ for tape(s):$([ "$TAPES" = "$ALL_TAPES" ] && echo " all" || echo " $TAPES")"
for tape in $TAPES; do
    case "$tape" in
        agents)      echo "    thurbox-demo.{gif,mp4}" ;;
        automations) echo "    automations-demo.{gif,mp4}" ;;
        tasks)       echo "    tasks-demo.{gif,mp4}" ;;
        search)      echo "    search-demo.{gif,mp4}" ;;
        *)           echo "    thurbox-$tape.{gif,mp4}" ;;
    esac
done
