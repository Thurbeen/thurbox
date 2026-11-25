#!/usr/bin/env bash
set -euo pipefail

log() { printf '[tutorial] %s\n' "$*" >&2; }
die() { printf '[tutorial] FATAL: %s\n' "$*" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="${1:-$REPO_ROOT/media/tutorial}"
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

THEME="${THEME:-doom}"
COLS="${COLS:-140}"
ROWS="${ROWS:-38}"
FONT_SIZE="${FONT_SIZE:-18}"
# agg's 1.4 default leaves a gap between rows' `│`; 1.0 clips descenders.
LINE_HEIGHT="${LINE_HEIGHT:-1.2}"
FONT_DIR="${FONT_DIR:-$REPO_ROOT/target/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono,Noto Sans Symbols 2}"
SOCK="thurbox-tutorial-rec"

for bin in cargo git tmux agg ffmpeg python3 jq; do
    command -v "$bin" >/dev/null 2>&1 || die "$bin not found on PATH"
done

fetch_font() {
    local dest="$FONT_DIR/$1"
    [ -f "$dest" ] && return 0
    curl -fsSL -o "$dest" "$2" \
        || { rm -f "$dest"; log "warning: could not fetch $1 — falling back"; }
}

ensure_fonts() {
    mkdir -p "$FONT_DIR"
    fetch_font "JetBrainsMono-Regular.ttf" \
        "https://github.com/google/fonts/raw/main/ofl/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf"
    fetch_font "NotoSansSymbols2-Regular.ttf" \
        "https://github.com/google/fonts/raw/main/ofl/notosanssymbols2/NotoSansSymbols2-Regular.ttf"
    if ! find "$FONT_DIR" -maxdepth 1 -name '*.ttf' | grep -q .; then
        local found
        found=$(find "$HOME/.local/share/fonts" /usr/share/fonts /usr/local/share/fonts \
            -name 'DejaVuSansMono*.ttf' 2>/dev/null | head -n 4)
        if [ -n "$found" ]; then
            printf '%s\n' "$found" | xargs -r -I{} cp {} "$FONT_DIR/"
        else
            log "warning: no font found for agg — the screenshots may be blank"
        fi
    fi
}
ensure_fonts

log "Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux --bin workspace-git >&2
THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
[ -x "$THURBOX_BIN" ] || die "dev binary not found at $THURBOX_BIN"

HAS_CLAUDE=0
command -v claude >/dev/null 2>&1 && HAS_CLAUDE=1

# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh

HOME_LINK="${HOME_LINK:-/tmp/tutorial-home}"
if [ -e "$HOME_LINK" ] && [ ! -L "$HOME_LINK" ]; then
    die "$HOME_LINK exists and is not a symlink — refusing to touch it"
fi
ln -sfn "$HOME" "$HOME_LINK"
export HOME="$HOME_LINK"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
mkdir -p "$CFG_DIR"
tbx_sandbox_runtime
unset FLEET_CONFIG

cleanup() {
    tmux -L "$SOCK" kill-server >/dev/null 2>&1 || true
    tbx_sandbox_teardown
    if [ -L "$HOME_LINK" ]; then rm -f "$HOME_LINK"; fi
}
trap cleanup EXIT INT TERM

path_without_wsl_exe_so_no_run_on_step_shifts_the_keys() {
    printf '%s' "$1" | tr ':' '\n' | grep -v '^/mnt/[a-z]/' | paste -sd: -
}
PATH=$(path_without_wsl_exe_so_no_run_on_step_shifts_the_keys "$PATH")
export PATH

CODE_DIR="$HOME/code"
NON_GIT_DIR_FOR_THE_BROWSE_MARKER="$CODE_DIR/scratch"
mkdir -p "$NON_GIT_DIR_FOR_THE_BROWSE_MARKER"
for repo in api-server web-app; do
    dir="$CODE_DIR/$repo"
    mkdir -p "$dir/src"
    printf '# %s\n\nPart of the tutorial workspace.\n' "$repo" > "$dir/README.md"
    printf 'fn main() {\n    println!("%s");\n}\n' "$repo" > "$dir/src/main.rs"
    git init -q -b main "$dir"
    git -C "$dir" -c user.email=tutorial@thurbox -c user.name=tutorial add -A
    git -C "$dir" -c user.email=tutorial@thurbox -c user.name=tutorial \
        commit -q -m "chore: init $repo"
done

mkdir -p "$HOME/.local/bin"
if [ "$HAS_CLAUDE" -eq 0 ]; then
    log "claude not installed — a stand-in claude runs a shell"
    printf '#!/bin/sh\necho "tutorial stand-in agent"\nexec sh\n' > "$HOME/.local/bin/claude"
    chmod +x "$HOME/.local/bin/claude"
fi
export PATH="$HOME/.local/bin:$PATH"

export OPENCODE_DISABLE_AUTOUPDATE=true
export CODEX_DISABLE_UPDATE_CHECK=1
export npm_config_update_notifier=false
export NO_UPDATE_NOTIFIER=1

TUTORIAL_BRANCH="rate-limit"

# An agent that canonicalises its cwd looks its trust entry up under the resolved path.
trusted_paths=()
for path in "$CODE_DIR/api-server" "$CODE_DIR/web-app"; do
    trusted_paths+=("$path" "$(readlink -f "$path")")
done

jq -n '{hasCompletedOnboarding: true,
        projects: ($ARGS.positional
                   | map({(.): {hasTrustDialogAccepted: true}}) | add)}' \
    --args "${trusted_paths[@]}" > "$HOME/.claude.json"
if [ "$HAS_CLAUDE" -eq 1 ]; then
    # claude's self-install check reports "claude command missing" without this link.
    ln -sf "$(readlink -f "$(command -v claude)")" "$HOME/.local/bin/claude"
fi

printf '{"theme": "%s"}\n' "$THEME" > "$CFG_DIR/interface.json"

abort_unless_fleet_config_resolves_inside_the_sandbox() {
    case "$(readlink -f "$XDG_CONFIG_HOME")" in
        "$(readlink -f "$TBX_SANDBOX_ROOT")"/*) ;;
        *) die "XDG_CONFIG_HOME ($XDG_CONFIG_HOME) is outside the sandbox — aborting" ;;
    esac
    [ -z "${FLEET_CONFIG:-}" ] || die "FLEET_CONFIG is set — aborting"
}
abort_unless_fleet_config_resolves_inside_the_sandbox
fleet init >/dev/null || die "fleet init failed"
tbx_sandbox_fleet_up

# tmux holds Escape back without escape-time 0, and capture-pane -e re-emits only the colours tmux stored.
TMUX_CONF="$TBX_SANDBOX_ROOT/tmux.conf"
cat > "$TMUX_CONF" <<'TMUXCONF'
set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*:Tc"
set -g status off
set -g escape-time 0
TMUXCONF

tmux -L "$SOCK" -f "$TMUX_CONF" new-session -d -s tut -x "$COLS" -y "$ROWS" \
    "$THURBOX_BIN"

TARGET=tut

send()  { tmux -L "$SOCK" send-keys -t "$TARGET" "$@"; }
write() { tmux -L "$SOCK" send-keys -t "$TARGET" -l -- "$1"; }
# Ctrl+/ is 0x1F, which has no tmux key name.
search() { tmux -L "$SOCK" send-keys -t "$TARGET" -H 1f; }
run()   { write "$1"; send Enter; sleep "$2"; }

SHOT_INDEX=0
# tmux capture-pane leaves the cursor out, so its position is read separately.
shot() {
    local name="$1"
    SHOT_INDEX=$((SHOT_INDEX + 1))
    local stem
    stem=$(printf '%02d-%s' "$SHOT_INDEX" "$name")
    local work="$TBX_SANDBOX_ROOT/shots"
    mkdir -p "$work"

    tmux -L "$SOCK" capture-pane -p -e -N -t "$TARGET" > "$work/$stem.ansi"
    if [ -n "${TEXT_DIR:-}" ]; then
        mkdir -p "$TEXT_DIR"
        tmux -L "$SOCK" capture-pane -p -t "$TARGET" > "$TEXT_DIR/$stem.txt"
    fi
    local geom
    geom=$(tmux -L "$SOCK" display -p -t "$TARGET" \
        '#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{cursor_flag}')

    # shellcheck disable=SC2086
    python3 - "$work/$stem.ansi" "$work/$stem.cast" $geom <<'PYCAST'
import json
import sys

dump, out, cols, rows, cx, cy, cursor = sys.argv[1:8]
cols, rows, cx, cy = int(cols), int(rows), int(cx), int(cy)
lines = open(dump, encoding="utf-8").read().rstrip("\n").split("\n")
# Home + erase first: agg replays into a blank grid, and a payload that assumed
# one would leave whatever a previous line had set in force.
payload = "\x1b[H\x1b[2J" + "\r\n".join(lines)
payload += f"\x1b[{cy + 1};{cx + 1}H" if cursor == "1" else "\x1b[?25l"

with open(out, "w", encoding="utf-8") as f:
    header = {"version": 2, "width": cols, "height": rows,
              "timestamp": 0, "env": {"TERM": "xterm-256color"}}
    f.write(json.dumps(header) + "\n")
    f.write(json.dumps([0.0, "o", payload]) + "\n")
PYCAST

    # agg's resvg renderer chokes on a family name ending in a digit ("Noto Sans Symbols 2").
    agg --renderer fontdue --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" \
        --font-size "$FONT_SIZE" --line-height "$LINE_HEIGHT" \
        --last-frame-duration 1 \
        "$work/$stem.cast" "$work/$stem.gif" >/dev/null 2>&1 \
        || { log "warning: agg failed for $stem"; return 0; }
    ffmpeg -y -loglevel error -i "$work/$stem.gif" -frames:v 1 "$OUT_DIR/$stem.png" \
        || { log "warning: ffmpeg failed for $stem"; return 0; }
    log "  $stem.png"
}

log "Driving the TUI (${COLS}x${ROWS}, theme: $THEME) -> $OUT_DIR"
rm -f "$OUT_DIR"/*.png

sleep 6
shot first-launch

send C-n; sleep 3
shot place-picker

send Enter; sleep 2
shot repo-picker

send Tab; sleep 1
# shellcheck disable=SC2088
write "~/code/"; sleep 2
shot add-repo-path

send Tab; sleep 3
shot browse-directory

send Enter; sleep 3
shot repo-added

send M-w; sleep 2
shot worktree-mode

send Enter; sleep 3
shot base-branch

send Enter; sleep 2
write "$TUTORIAL_BRANCH"; sleep 2
shot session-name

send Enter; sleep 2
shot branch-name

send Enter; sleep 2
shot agent-picker

send Enter; sleep 12
shot session-running

send C-n; sleep 3
send Enter; sleep 2
shot repo-remembered
send Escape; sleep 1

send C-h; sleep 1

search; sleep 1
write "rate"; sleep 2
shot search
send Escape; sleep 1

send C-g; sleep 2
shot keybindings
send Escape; sleep 1

send C-q; sleep 3

tmux -L "$SOCK" -f "$TMUX_CONF" new-session -d -s cli -x "$COLS" -y "$ROWS" \
    "bash --norc"
TARGET=cli
sleep 1
run "PS1='\$ '; clear" 1
run "fleet list task" 3
shot cli

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' | wc -l | tr -d ' ')
log "Captured $count screenshot(s) in $OUT_DIR"
[ "$count" -eq 0 ] && die "no screenshots produced"
printf '%s\n' "$OUT_DIR"
