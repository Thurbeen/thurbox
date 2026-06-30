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
# Keep the viewport at/under ~1568px on the long edge: image inputs are
# downscaled to that cap, so a 1920x1080 capture is resampled and the TUI's fine
# detail (status glyphs, tree markers, swatches) blurs into illegibility for the
# analyze phase. 1500x940 with FontSize 18 yields ~135 cols (>= the 120-col
# threshold for the 3-panel layout) and renders 1:1, so text stays readable.
WIDTH=1500
HEIGHT=940
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

# --- Isolated environment (reuse the repo's single source of truth) ----------
# Don't hand-roll isolation: scripts/dev/lib/sandbox-env.sh is the shared
# dev-sandbox helper already used by the demo recorder + TUI smoke test.
# `tbx_sandbox_init_full fresh` gives FULL hermetic isolation — a throwaway
# mktemp root with fresh HOME + XDG_* — and crucially UNSETS any inherited
# THURBOX_CONFIG_DIR / THURBOX_DATA_DIR. paths.rs honors those ahead of XDG, so an
# inherited override (e.g. from a thurbox-dev direnv/sandbox shell) would silently
# defeat XDG isolation and make the dev binary read/WRITE your REAL db + config.
# `fresh` => the root is removed on teardown. Build the binaries BEFORE this (done
# above) so cargo still resolves your real ~/.cargo before HOME is overridden.
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh

SANDBOX="$TBX_SANDBOX_ROOT"
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
DB_FILE="$XDG_DATA_HOME/thurbox-dev/thurbox.db"
mkdir -p "$CFG_DIR" "$(dirname "$DB_FILE")"

cleanup() { tbx_sandbox_teardown; }
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

# --- Keybindings override (VHS can't type Ctrl+, , Ctrl+/, or F-keys) --------
# Remap the actions whose default chords VHS cannot emit to free, typeable
# Ctrl+<letter> chords so the tape can open them. The rendered panels are
# identical regardless of which key opens them.
#   GlobalSearch: Ctrl+/    -> Ctrl+A   OpenSettings: Ctrl+, -> Ctrl+X
#   ToggleReview: Ctrl+X/F7 -> Ctrl+R   (code-review view; remapping it here also
#                                        frees its default Ctrl+X for OpenSettings)
# RestartSession's default is also Ctrl+R, and a file-loaded override does NOT
# steal a chord from another action — so without moving RestartSession off Ctrl+R
# it shadows ToggleReview and the tape ends up restarting the session instead of
# opening the diff. The tape never restarts, so park it on a chord it won't press.
cat > "$CFG_DIR/keybindings.json" <<'EOF'
{
  "GlobalSearch": ["ctrl+a"],
  "OpenSettings": ["ctrl+x"],
  "ToggleReview": ["ctrl+r"],
  "RestartSession": ["ctrl+shift+r"]
}
EOF

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
DEMO_BASE_BRANCH=$(git -C "$DEMO_REPO" symbolic-ref --short HEAD)

# --- Seed sessions + tasks + an automation ----------------------------------
log "Seeding sessions for:$AGENTS"
for a in $AGENTS; do
    "$CLI_BIN" session create --name "$a" --repo-path "$DEMO_REPO" --agent "$a" >/dev/null
done

# Isolation tripwire: at this point (CLI-only, no TUI yet) the sandbox DB must
# hold EXACTLY the sessions we just seeded, all rooted under $SANDBOX. If the
# count is off or any cwd escapes the sandbox, isolation has broken — abort
# LOUDLY rather than read/pollute the caller's real thurbox database.
# shellcheck disable=SC2086 # $AGENTS is a space-separated list, split on purpose
want=$(printf '%s\n' $AGENTS | grep -c .)
got=$(sqlite3 "$DB_FILE" "SELECT count(*) FROM sessions WHERE deleted_at IS NULL" 2>/dev/null || echo "?")
escaped=$(sqlite3 "$DB_FILE" \
    "SELECT cwd FROM sessions WHERE deleted_at IS NULL AND cwd NOT LIKE '$SANDBOX%'" \
    2>/dev/null || true)
if [ "$got" != "$want" ] || [ -n "$escaped" ]; then
    die "sandbox isolation breach — refusing to continue.
  DB:       $DB_FILE
  expected: $want session(s), all under $SANDBOX
  found:    $got session(s)${escaped:+, incl. cwd(s) OUTSIDE the sandbox:
$escaped}
This means THURBOX_DATA_DIR/THURBOX_CONFIG_DIR (or similar) pointed the dev
binary at your real data. Aborting before any further writes."
fi

# A worktree session with a committed multi-file diff, created LAST so the TUI
# selects it on launch — the code-review view (F7 / Ctrl+R here) diffs
# <base>..HEAD of this worktree, so the review screenshots show a real diff.
# shellcheck disable=SC2086 # $AGENTS is a space-separated list, split on purpose
review_agent=$(printf '%s\n' $AGENTS | head -n1)
"$CLI_BIN" session create --name "review" --repo-path "$DEMO_REPO" \
    --agent "$review_agent" --worktree-branch "review/demo" \
    --base-branch "$DEMO_BASE_BRANCH" >/dev/null
REVIEW_WT=$(git -C "$DEMO_REPO" worktree list --porcelain \
    | awk '/^worktree /{p=substr($0,10)} $0=="branch refs/heads/review/demo"{print p}')
if [ -n "$REVIEW_WT" ]; then
    cat > "$REVIEW_WT/src/lib.rs" <<'EOF'
pub fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

pub fn mul(a: i64, b: i64) -> i64 {
    a.wrapping_mul(b)
}
EOF
    cat > "$REVIEW_WT/src/greet.rs" <<'EOF'
pub fn greet(name: &str) -> String {
    format!("hello, {name}!")
}
EOF
    cat > "$REVIEW_WT/src/main.rs" <<'EOF'
mod greet;

fn main() {
    println!("{}", greet::greet("sample-project"));
}
EOF
    git -C "$REVIEW_WT" -c user.email=ui@thurbox -c user.name=ui add -A
    git -C "$REVIEW_WT" -c user.email=ui@thurbox -c user.name=ui \
        commit -q -m "feat: add greeting + checked arithmetic"
fi

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
11-code-review.png|Code-review view: diff + changed-files column|code-review|Ctrl+X
12-code-review-comment.png|Code review: inline comment compose + classification|code-review|c
13-code-review-sidebyside.png|Code review: side-by-side diff layout|code-review|v
14-code-review-targets.png|Code review: target picker (branch/working/commit)|code-review|t
02-second-session.png|A different session selected|session-list|Ctrl+J
03-info-panel.png|Session info panel|info-panel|Ctrl+B
04-file-viewer.png|File viewer (repo tree)|file-viewer|Ctrl+E
05-tasks-panel.png|Tasks panel (todo list)|tasks|Ctrl+W
06-automations.png|Automations pane|automations|Ctrl+P
07-global-search.png|Global search strip|search|Ctrl+A + query
08-theme-picker.png|Theme picker|theme|Ctrl+Y
09-repo-picker.png|New-session / repo picker|new-session|Ctrl+N
10-keybindings.png|Keybindings help + editor|keybindings|Ctrl+G
11-settings-panel.png|Settings panel|settings|Ctrl+,
12-automation-editor.png|Automation editor (multi-line prompt)|automation-editor|Ctrl+P then Enter
13-task-editor.png|Task editor (in-pane)|task-editor|Ctrl+W then Enter
MANIFEST
    printf '\n]\n'
} > "$OUT_DIR/manifest.json"

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' 2>/dev/null | wc -l | tr -d ' ')
log "Captured $count screenshot(s)."
[ "$count" -eq 0 ] && { log "no screenshots produced"; exit 1; }

# stdout: the directory the report phase reads from.
printf '%s\n' "$OUT_DIR"
