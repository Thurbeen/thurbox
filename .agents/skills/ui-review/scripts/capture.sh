#!/usr/bin/env sh
set -eu

log()  { printf '[ui-review] %s\n' "$*" >&2; }
die()  { printf '[ui-review] FATAL: %s\n' "$*" >&2; exit 2; }

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SKILL_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(git -C "$SKILL_DIR" rev-parse --show-toplevel 2>/dev/null) \
    || die "not inside a git repo — run this from the thurbox repo"
cd "$REPO_ROOT"

OUT_DIR=""
DEMO_THEME="${DEMO_THEME:-doom}"
# Image inputs are downscaled past ~1568px; 1500x940 renders 1:1 at ~135 cols.
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
# VHS Screenshot paths are literal, with no env expansion.
OUT_DIR=$(CDPATH= cd -- "$OUT_DIR" && pwd)

missing=
for tool in cargo git tmux vhs; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[ -n "$missing" ] && die "missing required tool(s):$missing (vhs needs ffmpeg + ttyd)"

build_dev_binaries_with_real_home() {
    log "Building thurbox (dev) ..."
    cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux >&2
}
build_dev_binaries_with_real_home
THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
export THURBOX_BIN
[ -x "$THURBOX_BIN" ] || die "dev binary not found at $THURBOX_BIN"

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

# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh

SANDBOX="$TBX_SANDBOX_ROOT"
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
mkdir -p "$CFG_DIR"
tbx_sandbox_fleet
tbx_sandbox_fleet_up

cleanup() { tbx_sandbox_teardown; }
trap cleanup EXIT INT TERM

if [ "$STUB" -eq 1 ]; then
    printf '#!/bin/sh\necho ui-review stub agent\nexec sh\n' > "$CFG_DIR/stub-agent.sh"
    chmod +x "$CFG_DIR/stub-agent.sh"
fi

# VHS sends neither Ctrl+/, Ctrl+, nor an F-key, so search and settings move to free Ctrl+letter slots.
cat > "$CFG_DIR/ui.json" <<'EOF'
{
  "bindings": {
    "search.open": "ctrl+a",
    "settings.open": "ctrl+b"
  }
}
EOF

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

assert_fleet_config_inside_sandbox() {
    case "${FLEET_CONFIG:-}" in
        "$SANDBOX"/*) ;;
        *) die "sandbox isolation breach — FLEET_CONFIG (${FLEET_CONFIG:-unset}) is outside $SANDBOX" ;;
    esac
}
assert_fleet_config_inside_sandbox

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

seed_branch_session_selected_at_launch() {
    # shellcheck disable=SC2086
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
}
seed_branch_session_selected_at_launch

printf '{"theme": "%s"}\n' "$DEMO_THEME" > "$CFG_DIR/interface.json"

TAPE_SRC="$SKILL_DIR/tapes/screenshots.tape"
[ -f "$TAPE_SRC" ] || die "tape not found: $TAPE_SRC"
TAPE_RUN="$SANDBOX/screenshots.tape"
sed -e "s|__SHOT_DIR__|$OUT_DIR|g" \
    -e "s|__WIDTH__|$WIDTH|g" \
    -e "s|__HEIGHT__|$HEIGHT|g" \
    "$TAPE_SRC" > "$TAPE_RUN"

log "Running VHS (theme: $DEMO_THEME, ${WIDTH}x${HEIGHT}) -> $OUT_DIR"
vhs "$TAPE_RUN" >&2 || log "warning: VHS exited non-zero; capturing whatever PNGs exist"

manifest_entry_if_present() {
    [ -f "$OUT_DIR/$1" ] || return 0
    printf '  {"file":"%s","label":"%s","screen":"%s","keys":"%s"}' "$1" "$2" "$3" "$4"
}
{
    echo "["
    first=1
    while IFS='|' read -r f l s k; do
        [ -z "$f" ] && continue
        entry=$(manifest_entry_if_present "$f" "$l" "$s" "$k") || true
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
09-settings.png|Settings: core + mod settings|settings|Ctrl+,
10-interface-tab.png|Settings: the Interface tab (mod inventory)|interface|Ctrl+, then ]
11-plugin-off.png|The arrangement after a pane is turned off|interface|space then Esc
12-shell-pane.png|Per-session shell pane|shell|Ctrl+T
MANIFEST
    printf '\n]\n'
} > "$OUT_DIR/manifest.json"

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' 2>/dev/null | wc -l | tr -d ' ')
log "Captured $count screenshot(s)."
[ "$count" -eq 0 ] && { log "no screenshots produced"; exit 1; }

printf '%s\n' "$OUT_DIR"
