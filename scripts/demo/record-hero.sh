#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

FONT_DIR="${FONT_DIR:-$HOME/.local/share/fonts}"
# agg 1.x draws no text when the Noto family is named too; it falls back to FONT_DIR alone.
FONT_FAMILY="${FONT_FAMILY:-JetBrainsMono Nerd Font Mono}"
COLS="${COLS:-160}"
ROWS="${ROWS:-45}"
FONT_SIZE="${FONT_SIZE:-20}"
GIF_FONT_SIZE="${GIF_FONT_SIZE:-12}"
FPS="${FPS:-20}"
THEME="${THEME:-doom}"
BOOT_SECS="${BOOT_SECS:-12}"
POSTER_AT="${POSTER_AT:-3}"
# AF_UNIX socket paths are length-limited, and HOME is under SBX.
SBX="${SBX:-/tmp/thurbox-hero}"
if [ -e "$SBX" ] && [ ! -e "$SBX/.thurbox-hero" ]; then
    echo "error: $SBX exists and is not a sandbox this script made; refusing to wipe it" >&2
    exit 1
fi

for bin in cargo git tmux python3 node asciinema agg ffmpeg; do
    command -v "$bin" >/dev/null 2>&1 || {
        echo "error: $bin not found on PATH" >&2
        exit 1
    }
done
case "$(asciinema --version 2>/dev/null)" in
    "asciinema 2."*) ;;
    *)
        echo "error: asciinema 2.x required (uv tool install 'asciinema==2.4.0')" >&2
        exit 1
        ;;
esac
[ -d "$FONT_DIR" ] || {
    echo "error: FONT_DIR '$FONT_DIR' does not exist (agg ships no font)" >&2
    exit 1
}

agents=()
for a in claude codex; do
    command -v "$a" >/dev/null 2>&1 && agents+=("$a")
done
[ ${#agents[@]} -gt 0 ] || {
    echo "error: neither claude nor codex is on PATH" >&2
    exit 1
}
agent_for() { echo "${agents[$(($1 % ${#agents[@]}))]}"; }

echo "==> building thurbox (dev)"
(cd "$ROOT" && cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux --bin workspace-git) || exit 1

REAL_HOME="$HOME"
REC_SOCKET="thurbox-hero-rec"
export TMUX_TMPDIR="$SBX/tmux"
export HOME="$SBX/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
export THURBOX_CONFIG_DIR="$XDG_CONFIG_HOME/thurbox"
export THURBOX_DATA_DIR="$XDG_DATA_HOME/thurbox"
export THURBOX_SOCKET="thurbox-hero"
unset THURBOX_SOCKET_FOR TMUX
PATH="$SBX/bin:$ROOT/target/debug:$PATH"
export PATH
export OPENCODE_DISABLE_AUTOUPDATE=true CODEX_DISABLE_UPDATE_CHECK=1 \
    npm_config_update_notifier=false NO_UPDATE_NOTIFIER=1

export TBX_REPO_ROOT="$ROOT"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$ROOT/scripts/dev/lib/sandbox-env.sh"

cleanup() {
    tmux -L "$REC_SOCKET" kill-server 2>/dev/null
    _tbx_stop_fleet "$SBX"
    tmux -L "$THURBOX_SOCKET" kill-server 2>/dev/null
    tmux -L runtime-mux kill-server 2>/dev/null
    pkill -f "^$SBX/" 2>/dev/null
    [ -n "${KEEP_SANDBOX:-}" ] || for _ in 1 2 3 4 5; do
        rm -rf "$SBX" 2>/dev/null && break
        sleep 1
    done
}
KEEP_SANDBOX= cleanup
# bash resumes the script after a trapped signal's handler returns, so INT and TERM exit.
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$TMUX_TMPDIR" "$HOME" "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR" "$SBX/bin"
touch "$SBX/.thurbox-hero"

export TBX_SANDBOX_ROOT="$SBX"
tbx_sandbox_fleet
tbx_sandbox_fleet_up

cat > "$THURBOX_CONFIG_DIR/settings.toml" <<'TOML'
[features]
version_check = false
auto_update = false
TOML

git_q() { git -c user.email=demo@example.invalid -c user.name=demo -c commit.gpgsign=false "$@"; }
make_repo() {
    git init -q -b main "$1"
    printf '# %s\n' "$(basename "$1")" > "$1/README.md"
    git_q -C "$1" add -A
    git_q -C "$1" commit -qm init
}
make_repo "$HOME/thurbox"
make_repo "$HOME/website"
make_repo "$HOME/api-gateway"
make_repo "$HOME/ml-pipeline"

trusted=("$HOME/thurbox" "$HOME/website" "$HOME/api-gateway" "$HOME/ml-pipeline" "$SBX/fleet/workspaces")
seed_agent_home() {
    local home="$1"
    if [ -f "$REAL_HOME/.codex/auth.json" ]; then
        mkdir -p "$home/.codex"
        cp "$REAL_HOME/.codex/auth.json" "$home/.codex/auth.json"
        {
            printf 'check_for_update_on_startup = false\n\n'
            for p in "${trusted[@]}"; do
                printf '[projects."%s"]\ntrust_level = "trusted"\n\n' "$p"
            done
        } > "$home/.codex/config.toml"
    fi
    python3 - "$home/.claude.json" "${trusted[@]}" <<'PY'
import json, sys
json.dump({"hasCompletedOnboarding": True,
           "projects": {p: {"hasTrustDialogAccepted": True} for p in sys.argv[2:]}},
          open(sys.argv[1], "w"))
PY
}
seed_agent_home "$HOME"
# claude prints a line under its prompt asking for tmux focus events until they are on.
echo 'set -g focus-events on' > "$HOME/.tmux.conf"

i=0
create() {
    local name="$1" repo="$2" branch="$3" agent
    agent="$(agent_for $i)"
    [ "$agent" = claude ] && agent=claude-code
    echo "==> session $name"
    tbx_fleet_session "$name" "git+file://$repo?branch=$branch#main" "$agent" || exit 1
    i=$((i + 1))
}
create fix-osc52-tmux "$HOME/thurbox" fix/osc52-tmux
create add-wsl-host-tests "$HOME/thurbox" test/wsl-hosts
create perf-session-order "$HOME/thurbox" perf/session-order
create docs-landing-copy "$HOME/website" docs/landing-copy
create rate-limit-middleware "$HOME/api-gateway" feat/rate-limit
create fix-auth-refresh "$HOME/api-gateway" fix/auth-refresh
create retrain-nightly "$HOME/ml-pipeline" ci/retrain-nightly

python3 - "$THURBOX_CONFIG_DIR/interface.json" "$THEME" fix-osc52-tmux add-wsl-host-tests \
    perf-session-order docs-landing-copy rate-limit-middleware fix-auth-refresh \
    retrain-nightly <<'PY' || exit 1
import json, sys
json.dump({"theme": sys.argv[2], "order": sys.argv[3:]}, open(sys.argv[1], "w"), indent=2)
PY

[ -n "${SETUP_ONLY:-}" ] && {
    echo "sandbox ready at $SBX (SETUP_ONLY); env: HOME=$HOME TMUX_TMPDIR=$TMUX_TMPDIR"
    trap - EXIT INT TERM
    exit 0
}

CAST="$SBX/hero.cast"
TRIMMED="$SBX/trimmed.cast"
GIF="$SBX/hero.gif"

echo "==> recording thurbox (${COLS}x${ROWS})"
# A new tmux server starts its panes from its own environment, not this script's.
tmux -L "$REC_SOCKET" new-session -d -x "$COLS" -y "$ROWS" -c "$HOME" -s r \
    "env TERM=xterm-256color HOME=$HOME TMUX_TMPDIR=$TMUX_TMPDIR \
     XDG_CONFIG_HOME=$XDG_CONFIG_HOME XDG_DATA_HOME=$XDG_DATA_HOME \
     XDG_STATE_HOME=$XDG_STATE_HOME XDG_CACHE_HOME=$XDG_CACHE_HOME \
     THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR THURBOX_DATA_DIR=$THURBOX_DATA_DIR \
     THURBOX_SOCKET=$THURBOX_SOCKET FLEET_CONFIG=$FLEET_CONFIG PATH=$PATH \
     asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS -c thurbox '$CAST'"
START=$SECONDS

send() { tmux -L "$REC_SOCKET" send-keys -t r "$@"; }
# SGR mouse reports, 1-based cells: a right press and release, and motion with no button.
right_click() {
    send -l $'\e[<2;'"$1;$2M"
    send -l $'\e[<2;'"$1;$2m"
}
hover() { send -l $'\e[<35;'"$1;$2M"; }
type_slowly() {
    local s="$1" k
    for ((k = 0; k < ${#s}; k++)); do
        send -l "${s:k:1}"
        sleep 0.25
    done
}

focus_list() { send C-h; }
walk_sessions_without_leaving_the_agent() {
    for _ in 1 2 3; do
        send C-j
        sleep 2
    done
}
open_a_session_in_another_repository() {
    focus_list
    sleep 1.2
    send j
    sleep 0.6
    send j
    sleep 0.6
    send Enter
    sleep 3
    focus_list
    sleep 0.8
    send j
    sleep 0.6
    send Enter
    sleep 3
}
fold_one_group_then_all_then_unfold() {
    focus_list
    sleep 0.8
    send g
    sleep 1
    send h
    sleep 1.5
    send H
    sleep 1.8
    send L
    sleep 1.8
}
walk_rate_limit_middleware_context_menu() {
    right_click 14 12
    sleep 1.2
    for y in 13 14 15 16; do
        hover 22 "$y"
        sleep 0.6
    done
    sleep 0.6
    send Escape
    sleep 1.2
}
search_every_agent_screen() {
    send C-_
    sleep 1
    type_slowly auth
    sleep 2.5
    send Escape
    sleep 2
}

sleep "$BOOT_SECS"
sleep 2.5
walk_sessions_without_leaving_the_agent
open_a_session_in_another_repository
fold_one_group_then_all_then_unfold
walk_rate_limit_middleware_context_menu
search_every_agent_screen

END=$((SECONDS - START))
send C-q
sleep 4

[ -s "$CAST" ] || {
    echo "error: no cast recorded at $CAST" >&2
    exit 1
}

node "$ROOT/scripts/demo/trim-cast.mjs" "$CAST" "$TRIMMED" "$BOOT_SECS" "$END" || exit 1

echo "==> rasterising with agg"
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size "$FONT_SIZE" \
    --fps-cap "$FPS" --theme asciinema "$TRIMMED" "$GIF" || exit 1

echo "==> encoding media/thurbox-demo.{gif,mp4} and the poster"
# `-r $FPS`: ffmpeg reads GIF frame delays as a ~100 fps variable rate.
ffmpeg -y -loglevel error -i "$GIF" -r "$FPS" \
    -c:v libx264 -preset slow -crf 28 -pix_fmt yuv420p -movflags +faststart \
    -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2" "$ROOT/media/thurbox-demo.mp4" || exit 1
# agg emits a frame only on change; an ffmpeg rescale would make every tick a full frame.
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size "$GIF_FONT_SIZE" \
    --fps-cap "$FPS" --theme asciinema "$TRIMMED" "$ROOT/media/thurbox-demo.gif" || exit 1
ffmpeg -y -loglevel error -ss "$POSTER_AT" -i "$GIF" -frames:v 1 -c:v libwebp -quality 88 \
    "$ROOT/website/assets/thurbox-demo-poster.webp" || exit 1

ls -la "$ROOT/media/thurbox-demo.gif" "$ROOT/media/thurbox-demo.mp4" \
    "$ROOT/website/assets/thurbox-demo-poster.webp"
echo "publish: scripts/demo/publish-media.sh media/thurbox-demo.gif media/thurbox-demo.mp4"
