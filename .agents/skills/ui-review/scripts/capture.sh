#!/usr/bin/env sh
# ui-review capture: drive the REAL thurbox TUI in an isolated sandbox and take a
# PNG screenshot of each major screen/panel using VHS.
#
# VHS `Screenshot` directives emit the stills. Nothing here can touch your real
# thurbox sessions, tmux server, or agent accounts:
#
#   * HOME + XDG_{DATA,CONFIG,STATE,CACHE}_HOME point at a throwaway mktemp dir, so
#     agents (if any) boot fresh and the dev DB/config live in the sandbox.
#   * TMUX_TMPDIR points at a throwaway dir, so the `thurbox-dev` tmux server lives
#     in its own socket directory and cleanup can't kill sessions you have running.
#   * The dev binary (0.0.0-dev => dev_build cfg) uses the `thurbox-dev` socket/dirs.
#
# Stub-agent fallback: if none of claude/codex/opencode are on PATH, a trivial
# stub agent (a long-lived shell) is registered so the TUI still spawns a session and
# renders every panel.
#
# Usage:  capture.sh [OUTPUT_DIR] [--theme NAME] [--width N] [--height N]
#
#   OUTPUT_DIR  where PNGs + manifest.json go (default: <repo>/target/ui-review/screenshots)
#   --theme     thurbox TUI theme string (default: doom; e.g. "Tokyo Night")
#   --width     VHS viewport width  in px (default 1500; keep >=120 cols equivalent)
#   --height    VHS viewport height in px (default 940)
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
for tool in cargo git tmux vhs; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[ -n "$missing" ] && die "missing required tool(s):$missing (vhs needs ffmpeg + ttyd)"

# --- Build the dev binaries (BEFORE the HOME override so cargo finds ~/.cargo) -
log "Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux >&2
THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
export THURBOX_BIN
[ -x "$THURBOX_BIN" ] || die "dev binary not found at $THURBOX_BIN"

# --- Which agent CLIs are available? ----------------------------------------
AGENTS=
for a in claude opencode codex; do
    command -v "$a" >/dev/null 2>&1 && AGENTS="$AGENTS $a"
done
STUB=0
if [ -z "$AGENTS" ]; then
    log "no real agent CLI found — using a stub agent so panels still render"
    AGENTS="stub"
    STUB=1
fi

# --- Isolated environment (reuse the repo's single source of truth) ----------
# Don't hand-roll isolation: scripts/dev/lib/sandbox-env.sh is the shared
# dev-sandbox helper already used by the demo recorder.
# `tbx_sandbox_init_full fresh` gives FULL hermetic isolation — a throwaway
# mktemp root with fresh HOME + XDG_* — and crucially UNSETS any inherited
# THURBOX_CONFIG_DIR / THURBOX_DATA_DIR. paths.rs honors those ahead of XDG, so an
# inherited override (e.g. from a thurbox-dev direnv/sandbox shell) would silently
# defeat XDG isolation and make the dev binary read/WRITE your REAL config.
# `fresh` => the root is removed on teardown. Build the binaries BEFORE this (done
# above) so cargo still resolves your real ~/.cargo before HOME is overridden.
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh

SANDBOX="$TBX_SANDBOX_ROOT"
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
mkdir -p "$CFG_DIR"
# The sessions are this sandbox's Fleet Tasks (exports FLEET_CONFIG).
tbx_sandbox_fleet

cleanup() { tbx_sandbox_teardown; }
trap cleanup EXIT INT TERM

# The stub as a program Fleet can run: a harness takes no arguments.
if [ "$STUB" -eq 1 ]; then
    printf '#!/bin/sh\necho ui-review stub agent\nexec sh\n' > "$CFG_DIR/stub-agent.sh"
    chmod +x "$CFG_DIR/stub-agent.sh"
fi

# --- Keybinding overrides (VHS can't type Ctrl+, , Ctrl+/, or F-keys) --------
# Two of the screens under review open on chords VHS cannot emit:
#   search.open   — really Ctrl+/ (and Ctrl+7 / Ctrl+_, which the kernel folds
#                   into it); no terminal sends any of them reliably.
#   settings.open — really Ctrl+, or F6, and VHS sends neither an F-key nor a
#                   Ctrl+punctuation chord.
# Ctrl+A and Ctrl+B are the two free Ctrl+<letter> slots: every other letter is
# claimed by the kernel (c/d/g/h/l/n/q/s/v/x/y/z) or by a plugin
# (f/j/k/o/p/r/t). A rebinding that collides is a conflict for the registry to
# resolve rather than an override, so picking a free slot matters.
#
# The kernel reads rebindings from `ui.json` as `bindings: { action: chord }` —
# the same file that carries trust and the disabled set, because all three are
# *user decisions*. (v1 used a separate keybindings.json holding arrays of chords
# keyed by CamelCase action names; neither the file nor those names exist now.)
# Written before first launch so the registry has it on the first frame.
cat > "$CFG_DIR/ui.json" <<'EOF'
{
  "bindings": {
    "search.open": "ctrl+a",
    "settings.open": "ctrl+b"
  }
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

# --- Seed sessions + an automation -------------------------------------------
# Isolation tripwire: the sessions are Tasks in the store FLEET_CONFIG names, so
# a FLEET_CONFIG outside the sandbox would seed — and screenshot — your real
# Fleet. Abort before any write.
case "${FLEET_CONFIG:-}" in
    "$SANDBOX"/*) ;;
    *) die "sandbox isolation breach — FLEET_CONFIG (${FLEET_CONFIG:-unset}) is outside $SANDBOX" ;;
esac

# Fleet runs claude and codex from its own harness profiles, and any other name
# as the program of that name.
harness_for() {
    case "$1" in
        claude) echo claude-code ;;
        stub) echo "$CFG_DIR/stub-agent.sh" ;;
        *) echo "$1" ;;
    esac
}
log "Seeding sessions for:$AGENTS"
for a in $AGENTS; do
    tbx_fleet_session "$a" "$DEMO_REPO" "$(harness_for "$a")" \
        || die "creating session '$a' failed"
done

# A session on a branch of its own, created LAST so the TUI selects it on
# launch. The branch is what the session list renders beside the name and what
# the search strip matches on, so a session that actually has one keeps those
# screens honest. Fleet runs a session in the directory it is given, so the
# worktree is made here.
# shellcheck disable=SC2086 # $AGENTS is a space-separated list, split on purpose
review_agent=$(printf '%s\n' $AGENTS | head -n1)
REVIEW_WT="$SANDBOX/worktrees/review"
git -C "$DEMO_REPO" worktree add -q -b review/demo "$REVIEW_WT" "$DEMO_BASE_BRANCH"
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
tbx_fleet_session review "$REVIEW_WT" "$(harness_for "$review_agent")" \
    || die "creating session 'review' failed"

# Automations no longer have a pane, so nothing below is screenshotted. One is
# seeded anyway: search reads past it, and empty state is a less honest thing to
# review than a populated one.
log "Seeding an automation"
"$CLI_BIN" automation create --name "nightly-triage" --trigger daily \
    --time "09:00" --repo "$DEMO_REPO" \
    --prompt "Triage failing CI and summarize blockers" >/dev/null 2>&1 || true

# --- Persist the TUI theme so the launched TUI starts in it ------------------
printf '{"theme": "%s"}\n' "$DEMO_THEME" > "$CFG_DIR/interface.json"

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
01-session-list.png|Session list + agent terminal (default view)|session-list|launch
02-second-session.png|A different session selected|session-list|Ctrl+J
03-search.png|Search strip, matches highlighted in the pane|search|Ctrl+/ + query
04-theme-picker.png|Theme picker (36 palettes, grouped)|theme|Ctrl+Y
05-theme-filter.png|Theme picker filtered behind /|theme|Ctrl+Y then / + query
06-new-session-repo.png|New session: repo picker|new-session|Ctrl+N
07-new-session-name.png|New session: naming step|new-session|Ctrl+N then Enter
08-help.png|Keybindings help, rendered from the registry|keybindings|Ctrl+G
09-settings.png|Settings: core + plugin settings|settings|Ctrl+,
10-interface-tab.png|Settings: the Interface tab (plugin inventory)|interface|Ctrl+, then ]
11-plugin-off.png|The arrangement after a pane is turned off|interface|space then Esc
12-shell-pane.png|Per-session shell pane|shell|Ctrl+T
MANIFEST
    printf '\n]\n'
} > "$OUT_DIR/manifest.json"

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' 2>/dev/null | wc -l | tr -d ' ')
log "Captured $count screenshot(s)."
[ "$count" -eq 0 ] && { log "no screenshots produced"; exit 1; }

# stdout: the directory the report phase reads from.
printf '%s\n' "$OUT_DIR"
