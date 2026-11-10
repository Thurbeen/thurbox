#!/usr/bin/env bash
# Regenerate the hero demo — the clip the README opens with and the website's
# first video:
#
#   media/thurbox-demo.gif                    (README)
#   media/thurbox-demo.mp4                    (copied into website/assets/ at
#                                              deploy time by pages.yml)
#   website/assets/thurbox-demo-poster.webp   (committed; the poster frame)
#
#   scripts/demo/record-hero.sh               # the whole regeneration
#
# What it shows, in order: the session list grouped by repository; walking the
# sessions; opening one; folding and unfolding groups; a session's context menu;
# search. Every session is a Fleet Task on this machine: runtime-mux serves
# local hosts only, so the clip has no remote host.
#
# Not a VHS tape. VHS cannot press a mouse button, and the context menu opens on
# a right press, nor can it send F-keys or Ctrl+punctuation, so a tape has to
# rebind half the chords it shows. Here thurbox runs under asciinema inside a
# private tmux server and `tmux send-keys` presses the real keys — a right press
# is the SGR mouse report a terminal would send. agg rasterises the cast.
#
# Isolation: HOME, XDG, thurbox's config/data and the Fleet store all point into
# $SBX, and every tmux server lives under its TMUX_TMPDIR, so the teardown
# cannot reach a server you have running. The agents boot with no
# history; codex's auth token is copied in (it shows no identity when signed
# in), claude is left signed out (signed in, it prints the account's
# organisation name).
#
# Requirements: cargo, git, tmux, python3, node, asciinema **2.x**
# (trim-cast.mjs reads v2's absolute timestamps), agg, ffmpeg; claude and/or
# codex for the agent panes. agg ships no font, so FONT_DIR must hold JetBrains
# Mono Nerd Font Mono (https://github.com/ryanoasis/nerd-fonts/releases ->
# JetBrainsMono.tar.xz) and Noto Sans Symbols 2 (github.com/notofonts), which
# has the worktree mark `⑂` (U+2442) no Nerd Font carries. Name only the first:
# agg falls back to the rest of FONT_DIR on its own, and listing the Noto
# family as well left every frame with no text at all (agg 1.x).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

FONT_DIR="${FONT_DIR:-$HOME/.local/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrainsMono Nerd Font Mono}"
COLS="${COLS:-160}"
ROWS="${ROWS:-45}"
FONT_SIZE="${FONT_SIZE:-20}"
# The README GIF's; GitHub shows it no wider than ~900px anyway.
GIF_FONT_SIZE="${GIF_FONT_SIZE:-12}"
FPS="${FPS:-20}"
THEME="${THEME:-doom}"
# Boot is cut from the clip: thurbox adopting the sessions and the agents
# drawing their first screen is not something to watch.
BOOT_SECS="${BOOT_SECS:-12}"
POSTER_AT="${POSTER_AT:-4}"
# Short and fixed: AF_UNIX socket paths are length-limited, and HOME is under it.
SBX="${SBX:-/tmp/thurbox-hero}"
# Every run starts with `rm -rf "$SBX"`, so an SBX pointed at a directory this
# script did not make (`SBX=~`) must be refused rather than emptied.
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
# The session -> agent pairing below cycles through what is installed.
agent_for() { echo "${agents[$(($1 % ${#agents[@]}))]}"; }

echo "==> building thurbox (dev)"
(cd "$ROOT" && cargo build --bin thurbox --bin thurbox-cli --bin fleet --bin runtime-mux) || exit 1

REAL_HOME="$HOME"
REC_SOCKET="thurbox-hero-rec"
export TMUX_TMPDIR="$SBX/tmux"
export HOME="$SBX/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
export THURBOX_CONFIG_DIR="$XDG_CONFIG_HOME/thurbox"
export THURBOX_DATA_DIR="$XDG_DATA_HOME/thurbox"
# Names nothing else uses — never `thurbox`: the teardown kills these by name,
# and a socket directory that failed to apply must not make that name yours.
export THURBOX_SOCKET="thurbox-hero"
unset THURBOX_SOCKET_FOR TMUX
PATH="$SBX/bin:$ROOT/target/debug:$PATH"
export PATH
export OPENCODE_DISABLE_AUTOUPDATE=true CODEX_DISABLE_UPDATE_CHECK=1 \
    npm_config_update_notifier=false NO_UPDATE_NOTIFIER=1

# The sandbox helpers: Fleet's config and its teardown.
export TBX_REPO_ROOT="$ROOT"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$ROOT/scripts/dev/lib/sandbox-env.sh"

cleanup() {
    tmux -L "$REC_SOCKET" kill-server 2>/dev/null
    # `fleet up` (thurbox starts it) first, so it does not start the agents again.
    _tbx_stop_fleet "$SBX"
    tmux -L "$THURBOX_SOCKET" kill-server 2>/dev/null
    tmux -L runtime-mux kill-server 2>/dev/null
    # Whatever an agent daemonised out of its pane runs from a binary it
    # installed under the sandbox, so the path names it and nothing else.
    pkill -f "^$SBX/" 2>/dev/null
    # An agent killed with its server can still be writing its state for a
    # moment, which makes a single rm -rf race it.
    [ -n "${KEEP_SANDBOX:-}" ] || for _ in 1 2 3 4 5; do
        rm -rf "$SBX" 2>/dev/null && break
        sleep 1
    done
}
KEEP_SANDBOX= cleanup  # a kept sandbox is for inspecting the last run, not reusing it
# A trapped signal resumes the script once its handler returns, so INT and TERM
# exit (which runs the EXIT trap) instead of tearing down and carrying on.
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$TMUX_TMPDIR" "$HOME" "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR" "$SBX/bin"
touch "$SBX/.thurbox-hero"

# The sessions are Fleet Tasks, on a Fleet of the sandbox's own: the shared
# helper writes its config under $SBX and exports FLEET_CONFIG.
export TBX_SANDBOX_ROOT="$SBX"
tbx_sandbox_fleet

# Both flags reach the network: one puts an upgrade notice in the top band, the
# other replaces binaries on startup.
cat > "$THURBOX_CONFIG_DIR/settings.toml" <<'TOML'
[features]
version_check = false
auto_update = false
TOML

# --- Demo repositories ------------------------------------------------------------
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

# --- Agent first-run state --------------------------------------------------------
# Trust prompts and onboarding screens would otherwise be what every pane shows.
trusted=("$HOME/thurbox" "$HOME/website" "$HOME/api-gateway" "$HOME/ml-pipeline")
seed_agent_home() {
    local home="$1"
    if [ -f "$REAL_HOME/.codex/auth.json" ]; then
        mkdir -p "$home/.codex"
        cp "$REAL_HOME/.codex/auth.json" "$home/.codex/auth.json"
        {
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
# Without focus events claude prints a line under its prompt telling you to
# turn them on.
echo 'set -g focus-events on' > "$HOME/.tmux.conf"

# --- Sessions ---------------------------------------------------------------------
# Named after the work, so the list reads as one backlog in flight. Worktree
# sessions carry the branch mark in the list.
i=0
create() { # name repo branch
    local name="$1" repo="$2" branch="$3" agent wt
    agent="$(agent_for $i)"
    [ "$agent" = claude ] && agent=claude-code
    wt="$SBX/worktrees/$name"
    echo "==> session $name"
    git_q -C "$repo" worktree add -q -b "$branch" "$wt" main || exit 1
    tbx_fleet_session "$name" "$wt" "$agent" || exit 1
    i=$((i + 1))
}
create fix-osc52-tmux "$HOME/thurbox" fix/osc52-tmux
create add-wsl-host-tests "$HOME/thurbox" test/wsl-hosts
create perf-session-order "$HOME/thurbox" perf/session-order
create docs-landing-copy "$HOME/website" docs/landing-copy
create rate-limit-middleware "$HOME/api-gateway" feat/rate-limit
create fix-auth-refresh "$HOME/api-gateway" fix/auth-refresh
create retrain-nightly "$HOME/ml-pipeline" ci/retrain-nightly

# The theme, and the list in creation order: the context menu below is opened by
# a row's position.
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

# --- Record -----------------------------------------------------------------------
CAST="$SBX/hero.cast"
TRIMMED="$SBX/trimmed.cast"
GIF="$SBX/hero.gif"

echo "==> recording thurbox (${COLS}x${ROWS})"
# asciinema owns the pty, so `send-keys` reaches thurbox through it. The env is
# spelled out because a new tmux server starts its panes from its own
# environment, not this script's.
tmux -L "$REC_SOCKET" new-session -d -x "$COLS" -y "$ROWS" -c "$HOME" -s r \
    "env TERM=xterm-256color HOME=$HOME TMUX_TMPDIR=$TMUX_TMPDIR \
     XDG_CONFIG_HOME=$XDG_CONFIG_HOME XDG_DATA_HOME=$XDG_DATA_HOME \
     XDG_STATE_HOME=$XDG_STATE_HOME XDG_CACHE_HOME=$XDG_CACHE_HOME \
     THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR THURBOX_DATA_DIR=$THURBOX_DATA_DIR \
     THURBOX_SOCKET=$THURBOX_SOCKET FLEET_CONFIG=$FLEET_CONFIG PATH=$PATH \
     asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS -c thurbox '$CAST'"
START=$SECONDS

send() { tmux -L "$REC_SOCKET" send-keys -t r "$@"; }
# SGR mouse reports, 1-based cells — what a terminal sends for a right press and
# for the pointer moving with no button held.
right_click() {
    send -l $'\e[<2;'"$1;$2M"
    send -l $'\e[<2;'"$1;$2m"
}
hover() { send -l $'\e[<35;'"$1;$2M"; }
# Type like a person, so the search narrows visibly as each letter lands.
type_slowly() {
    local s="$1" k
    for ((k = 0; k < ${#s}; k++)); do
        send -l "${s:k:1}"
        sleep 0.25
    done
}

sleep "$BOOT_SECS"
sleep 2.5

# Walk this machine's sessions. Ctrl+J is global: the agent pane keeps the
# keyboard and follows the selection.
for _ in 1 2 3; do
    send C-j
    sleep 2
done

# Into the list (Ctrl+H), down onto another repository's session and open it.
send C-h
sleep 1.2
send j
sleep 0.6
send j
sleep 0.6
send Enter
sleep 3
# Opening hands the keyboard to the agent, so every list key below is preceded
# by Ctrl+H — without it they are typed into the agent's prompt.
send C-h
sleep 0.8
send j
sleep 0.6
send Enter
sleep 3

# Folding: one group, then every group, then all of them back.
send C-h
sleep 0.8
send g
sleep 1
send h
sleep 1.5
send H
sleep 1.8
send L
sleep 1.8

# A session's context menu: right press on rate-limit-middleware's row, and the
# pointer down its entries.
right_click 14 12
sleep 1.2
for y in 13 14 15 16; do
    hover 22 "$y"
    sleep 0.6
done
sleep 0.6
send Escape
sleep 1.2

# Search: names, agents, branches, and the text on every agent's screen.
send C-_
sleep 1
type_slowly auth
sleep 2.5
send Escape
sleep 2

END=$((SECONDS - START))
send C-q
sleep 4

[ -s "$CAST" ] || {
    echo "error: no cast recorded at $CAST" >&2
    exit 1
}

# Trimmed to the moment the screen settled, so frame one is fully painted.
node "$ROOT/scripts/demo/trim-cast.mjs" "$CAST" "$TRIMMED" "$BOOT_SECS" "$END" || exit 1

echo "==> rasterising with agg"
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size "$FONT_SIZE" \
    --fps-cap "$FPS" --theme asciinema "$TRIMMED" "$GIF" || exit 1

echo "==> encoding media/thurbox-demo.{gif,mp4} and the poster"
# `-r $FPS`: ffmpeg reads GIF frame delays as a ~100 fps variable rate.
ffmpeg -y -loglevel error -i "$GIF" -r "$FPS" \
    -c:v libx264 -preset slow -crf 28 -pix_fmt yuv420p -movflags +faststart \
    -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2" "$ROOT/media/thurbox-demo.mp4" || exit 1
# The README GIF is rendered a second time at a smaller font rather than scaled
# down from the first: agg only emits a frame when the screen changes, and
# re-encoding through ffmpeg turns that into one full frame per tick.
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size "$GIF_FONT_SIZE" \
    --fps-cap "$FPS" --theme asciinema "$TRIMMED" "$ROOT/media/thurbox-demo.gif" || exit 1
ffmpeg -y -loglevel error -ss "$POSTER_AT" -i "$GIF" -frames:v 1 -c:v libwebp -quality 88 \
    "$ROOT/website/assets/thurbox-demo-poster.webp" || exit 1

ls -la "$ROOT/media/thurbox-demo.gif" "$ROOT/media/thurbox-demo.mp4" \
    "$ROOT/website/assets/thurbox-demo-poster.webp"
