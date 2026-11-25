#!/usr/bin/env bash
# asciinema 3 writes asciicast v3; trim-cast.mjs reads v2's absolute timestamps, so record with 2.x.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

FONT_DIR="${FONT_DIR:-$HOME/.local/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrainsMono Nerd Font,DejaVuSansM Nerd Font}"
COLS="${COLS:-160}"
ROWS="${ROWS:-44}"
FONT_SIZE="${FONT_SIZE:-20}"
FPS="${FPS:-25}"
THEME="${THEME:-doom}"
DOOM_PLUGIN_SRC="${DOOM_PLUGIN_SRC:-git+https://github.com/Thurbeen/thurbox-doom}"

BOOT_SECS="${BOOT_SECS:-10}"
DOOM_SECS_AFTER_F5="${DOOM_SECS:-60}"
DOOM_ATTRACT_DEMO2_CUT_AFTER_F5_SECS=37
LOOPED_CLIP_SECS=21
START="${START:-$((BOOT_SECS + DOOM_ATTRACT_DEMO2_CUT_AFTER_F5_SECS))}"
END="${END:-$((START + LOOPED_CLIP_SECS))}"
POSTER_SECS_INTO_TRIMMED_CLIP="${POSTER_AT:-3}"

SBX="${SBX:-/tmp/thurbox-doom-rec}"

SESSION_NAMES=(fix-osc52-tmux add-wsl-host-tests perf-session-order-cache docs-remote-hooks)
SESSION_BRANCHES=(fix/osc52-tmux test/wsl-hosts perf/session-order docs/remote-hooks)

for bin in thurbox thurbox-cli fleet runtime-mux node python3 asciinema agg ffmpeg ffprobe tmux git; do
    command -v "$bin" >/dev/null 2>&1 || {
        echo "error: $bin not found on PATH" >&2
        exit 1
    }
done
python3 -c 'import tomllib' 2>/dev/null || {
    echo "error: python3 has no tomllib (needs >= 3.11)" >&2
    exit 1
}
[ -d "$FONT_DIR" ] || {
    echo "error: FONT_DIR '$FONT_DIR' does not exist (agg ships no font)" >&2
    exit 1
}
[ "$END" -lt $((BOOT_SECS + DOOM_SECS_AFTER_F5)) ] || {
    echo "error: END ($END) is past the end of the recording ($((BOOT_SECS + DOOM_SECS_AFTER_F5))s)" >&2
    exit 1
}

# AF_UNIX socket paths are length-limited, so SBX stays short.
export TMUX_TMPDIR="$SBX/tmux"
export THURBOX_CONFIG_DIR="$SBX/config"
export THURBOX_DATA_DIR="$SBX/data"
export THURBOX_SOCKET="thurbox-doom"
unset THURBOX_SOCKET_FOR

export TBX_REPO_ROOT="$ROOT"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$ROOT/scripts/dev/lib/sandbox-env.sh"
export TBX_SANDBOX_ROOT="$SBX"

rm -rf "$SBX"
mkdir -p "$SBX"/{tmux,config,data}

cat > "$THURBOX_CONFIG_DIR/settings.toml" <<'SETTINGS'
[features]
version_check = false
auto_update = false
SETTINGS
REPO="$SBX/thurbox"
git init -q -b main "$REPO"
printf 'thurbox\n' > "$REPO/README.md"
git -C "$REPO" add -A
git -C "$REPO" -c user.email=demo@thurbox -c user.name=demo -c commit.gpgsign=false \
    commit -qm init

CAST="$SBX/doom.cast"
TRIMMED="$SBX/trimmed.cast"
GIF="$SBX/doom.gif"

cleanup() {
    _tbx_stop_fleet "$SBX"
    tmux -L thurbox-doom-rec kill-server 2>/dev/null
    tmux -L "$THURBOX_SOCKET" kill-server 2>/dev/null
    tmux -L runtime-mux kill-server 2>/dev/null
}
trap cleanup EXIT

paths=$(thurbox-cli config show --json 2>/dev/null | python3 -c '
import json, sys
paths = json.load(sys.stdin)["paths"]
print(paths["ui_dir"]); print(paths["mod_state_json"]); print(paths["interface_json"])
')
[ -n "$paths" ] || {
    echo "error: could not read the resolved paths from thurbox-cli config show" >&2
    exit 1
}
{
    IFS= read -r UI_DIR
    IFS= read -r PLUGIN_STATE
    IFS= read -r INTERFACE_JSON
} <<< "$paths"

echo "==> installing the doom plugin into $UI_DIR"
thurbox-cli mod install "$DOOM_PLUGIN_SRC" --text || exit 1

case "$(uname -s)" in
    Linux) plat_os=linux ;;
    Darwin) plat_os=macos ;;
    *) plat_os=unknown ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) plat_arch=x86_64 ;;
    arm64 | aarch64) plat_arch=aarch64 ;;
    *) plat_arch=unknown ;;
esac
ENGINE="$UI_DIR/thurbox-doom/engine/bin/$plat_os-$plat_arch/doom"
[ -x "$ENGINE" ] || {
    echo "error: the plugin ships no engine for $plat_os-$plat_arch" >&2
    echo "  the builds it ships are under $UI_DIR/thurbox-doom/engine/bin" >&2
    echo "  build one with 'cd $UI_DIR/thurbox-doom/engine/src && make'" >&2
    exit 1
}

echo "==> granting the program capability"
seed_the_grant_the_settings_modal_writes_from_mods_lock() {
    python3 - "$UI_DIR" "$PLUGIN_STATE" <<'PYTRUST'
import json, pathlib, sys, tomllib

ui_dir, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
lock = tomllib.loads((ui_dir / "mods.lock").read_text())
entries = lock.get("mod", [])
if len(entries) != 1:
    sys.exit(f"expected one installed plugin, found {len(entries)}")
entry = entries[0]
pane = entry["file"]
grant = {"pin": f"{entry['src']}@{entry['version']}", "digest": entry["files"][pane]}
out.write_text(json.dumps({"trusted": {str(ui_dir / pane): grant}}, indent=2))
print(f"trusting {pane} at {grant['pin']}")
PYTRUST
}
seed_the_grant_the_settings_modal_writes_from_mods_lock || exit 1

agents=()
for a in claude codex opencode agy; do
    command -v "$a" >/dev/null 2>&1 && agents+=("$a")
done
if [ ${#agents[@]} -eq 0 ]; then
    echo "warning: no agent CLI found — recording with an empty session list" >&2
else
    tbx_sandbox_fleet
    tbx_sandbox_fleet_up
    for i in "${!agents[@]}"; do
        [ "$i" -lt "${#SESSION_NAMES[@]}" ] || break
        echo "==> creating session ${SESSION_NAMES[$i]} (${agents[$i]})"
        wt="$SBX/worktrees/${SESSION_NAMES[$i]}"
        git -C "$REPO" worktree add -q -b "${SESSION_BRANCHES[$i]}" "$wt" main || exit 1
        tbx_fleet_session "${SESSION_NAMES[$i]}" "$wt" "${agents[$i]}" || exit 1
    done
fi

printf '{"theme": "%s"}\n' "$THEME" > "$INTERFACE_JSON" || exit 1

echo "==> recording thurbox (${COLS}x${ROWS})"
tmux -L thurbox-doom-rec new-session -d -x "$COLS" -y "$ROWS" -c "$REPO" -s r \
    "TMUX_TMPDIR=$TMUX_TMPDIR THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR \
     THURBOX_DATA_DIR=$THURBOX_DATA_DIR THURBOX_SOCKET=$THURBOX_SOCKET PATH=$PATH \
     FLEET_CONFIG=${FLEET_CONFIG:-} \
     asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS -c thurbox '$CAST'"

send() { tmux -L thurbox-doom-rec send-keys -t r "$@"; }

sleep "$BOOT_SECS"
send F5
sleep "$DOOM_SECS_AFTER_F5"
send C-q
sleep 6

[ -s "$CAST" ] || {
    echo "error: no cast recorded at $CAST" >&2
    exit 1
}

echo "==> trimming to ${START}-${END}s"
node "$ROOT/scripts/demo/trim-cast.mjs" "$CAST" "$TRIMMED" "$START" "$END"

echo "==> rasterising with agg (slow: a few minutes)"
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" \
    --font-size "$FONT_SIZE" --fps-cap "$FPS" --theme asciinema "$TRIMMED" "$GIF"

[ -s "$GIF" ] || {
    echo "error: agg produced no GIF at $GIF" >&2
    exit 1
}

echo "==> encoding media/doom-easter-egg.mp4"
# `-r $FPS`: ffmpeg reads GIF frame delays as a ~100 fps variable rate.
ffmpeg -y -loglevel error -i "$GIF" -r "$FPS" \
    -c:v libx264 -preset slow -crf 30 -pix_fmt yuv420p -movflags +faststart \
    "$ROOT/media/doom-easter-egg.mp4"

echo "==> encoding website/assets/doom-easter-egg-poster.webp"
ffmpeg -y -loglevel error -ss "$POSTER_SECS_INTO_TRIMMED_CLIP" -i "$GIF" -frames:v 1 \
    -c:v libwebp -quality 88 \
    "$ROOT/website/assets/doom-easter-egg-poster.webp"

echo "==> done"
ffprobe -v error -show_entries stream=width,height:format=duration -of default=nw=1 \
    "$ROOT/media/doom-easter-egg.mp4"
ls -la "$ROOT/media/doom-easter-egg.mp4" \
    "$ROOT/website/assets/doom-easter-egg-poster.webp"
echo
echo "for a local preview: cp media/doom-easter-egg.mp4 website/assets/"
echo "publish: scripts/demo/publish-media.sh media/doom-easter-egg.mp4"
echo "if the dimensions above changed, update doomVideo.width/height in website/js/main.js"
