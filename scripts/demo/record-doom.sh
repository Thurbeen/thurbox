#!/usr/bin/env bash
# Regenerate the website's `iddqd` easter-egg clip: Doom running *inside* a
# thurbox pane, via the pi agent and the pi-doom extension.
#
#   docs/media/doom-easter-egg.mp4              (copied into website/assets/ at
#                                                deploy time by pages.yml)
#   website/assets/doom-easter-egg-poster.webp  (committed; the poster frame)
#
# Unlike the other demos this is not a VHS tape. VHS drives a TUI through
# ttyd + a headless browser; here the whole point is a *nested* TUI (thurbox
# rendering pi rendering Doom), and the toolchain is lighter: asciinema records
# the real thurbox to an asciicast and agg rasterises it to frames. No browser.
#
# The clip is Doom's own attract demo, which needs no input: thurbox forwards
# key *presses* but not *releases* (src/main.rs requests only
# DISAMBIGUATE_ESCAPE_CODES and run_loop matches KeyEventKind::Press), so a held
# movement key would latch. Menus and cheats — anything tap-driven — do work.
#
# Requirements:
#   thurbox + thurbox-cli on PATH   (the binaries under test)
#   node >= 22.19, npx              (pi needs it)
#   pi + pi-doom                    npm i -g --ignore-scripts @earendil-works/pi-coding-agent
#                                   pi install git:github.com/badlogic/pi-doom
#   asciinema, agg, ffmpeg, tmux, git
#   a monospace TTF with box-drawing + block + braille coverage (agg has no
#   built-in font). Point FONT_DIR at it; a Nerd Font works:
#   https://github.com/ryanoasis/nerd-fonts/releases -> JetBrainsMono.tar.xz
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

FONT_DIR="${FONT_DIR:-$HOME/.local/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrainsMono Nerd Font,DejaVuSansM Nerd Font}"
COLS="${COLS:-160}"
ROWS="${ROWS:-44}"
FONT_SIZE="${FONT_SIZE:-20}"
FPS="${FPS:-25}"
# Seconds of attract demo to let run before quitting. Must outlast END below,
# or the trim window runs past the end of the recording.
DOOM_SECS="${DOOM_SECS:-28}"
# Window kept from the raw cast. thurbox boot + pi boot + /new + /doom lands the
# first Doom frame at ~33 s; START skips past it so frame 1 is already gameplay.
START="${START:-34.5}"
END="${END:-55.0}"
# Poster frame: a moment mid-clip with the Doom view and HUD well lit.
POSTER_AT="${POSTER_AT:-12}"

SBX="${SBX:-/tmp/thurbox-doom-rec}"

for bin in thurbox thurbox-cli node npx asciinema agg ffmpeg ffprobe tmux git; do
    command -v "$bin" >/dev/null 2>&1 || {
        echo "error: $bin not found on PATH" >&2
        exit 1
    }
done
[ -d "$FONT_DIR" ] || {
    echo "error: FONT_DIR '$FONT_DIR' does not exist (agg ships no font)" >&2
    exit 1
}

# Fully isolated: thurbox's own config/data plus a private TMUX_TMPDIR, so this
# never touches your real sessions or the release `thurbox` socket. Kept short —
# AF_UNIX socket paths are length-limited.
export TMUX_TMPDIR="$SBX/tmux"
export THURBOX_CONFIG_DIR="$SBX/config"
export THURBOX_DATA_DIR="$SBX/data"

rm -rf "$SBX"
mkdir -p "$SBX"/{tmux,config,data,repo}
git -C "$SBX/repo" init -q
printf 'thurbox\n' > "$SBX/repo/README.md"
git -C "$SBX/repo" add -A
git -C "$SBX/repo" -c user.email=demo@thurbox -c user.name=demo -c commit.gpgsign=false \
    commit -qm init

CAST="$SBX/doom.cast"
TRIMMED="$SBX/trimmed.cast"
GIF="$SBX/doom.gif"

# Both kills are scoped by the exported TMUX_TMPDIR above, which is why killing
# the `thurbox` socket by name here cannot reach your real thurbox server.
cleanup() {
    tmux -L thurbox-doom-rec kill-server 2>/dev/null
    tmux -L thurbox kill-server 2>/dev/null
}
trap cleanup EXIT

echo "==> creating a pi session"
thurbox-cli session create --name doom --agent pi --repo-path "$SBX/repo" --text 2>&1 | head -3

echo "==> recording thurbox (${COLS}x${ROWS})"
# asciinema owns the pty, so `tmux send-keys` below reaches thurbox through it.
tmux -L thurbox-doom-rec new-session -d -x "$COLS" -y "$ROWS" -c "$SBX/repo" -s r \
    "TMUX_TMPDIR=$TMUX_TMPDIR THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR \
     THURBOX_DATA_DIR=$THURBOX_DATA_DIR PATH=$PATH \
     asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS -c thurbox '$CAST'"

send() { tmux -L thurbox-doom-rec send-keys -t r "$@"; }

sleep 14                     # thurbox boots and adopts the session
send Enter                   # focus the agent terminal
sleep 10                     # pi finishes booting
# /new resets the transcript: it drops the "no models available" warning an
# unauthenticated pi prints and leaves a clean banner whose [Extensions] line
# credits pi-doom. Not /clear — that is not a pi command, so it is sent as a
# prompt and fails with an API-key error right in the shot.
send "/new"; sleep 2; send Enter; sleep 3
send "/doom"; sleep 3; send Enter
sleep "$DOOM_SECS"
send C-q                     # thurbox quit -> asciinema finalises the cast
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

# Guard explicitly: this script deliberately runs without `set -e` (the
# `| head` pipes would trip pipefail), so a failed agg would otherwise fall
# through and re-encode a stale GIF into the shipped mp4.
[ -s "$GIF" ] || {
    echo "error: agg produced no GIF at $GIF" >&2
    exit 1
}

echo "==> encoding docs/media/doom-easter-egg.mp4"
# The GIF is the intermediate agg gives us; per-frame palettes cost little here
# because Doom's own palette is 256 colours to begin with. `-r $FPS` is required,
# not cosmetic: ffmpeg reads GIF frame delays as a ~100 fps variable rate and
# would otherwise emit a bloated 100 fps mp4 full of duplicate frames.
ffmpeg -y -loglevel error -i "$GIF" -r "$FPS" \
    -c:v libx264 -preset slow -crf 30 -pix_fmt yuv420p -movflags +faststart \
    "$ROOT/docs/media/doom-easter-egg.mp4"

echo "==> encoding website/assets/doom-easter-egg-poster.webp"
ffmpeg -y -loglevel error -ss "$POSTER_AT" -i "$GIF" -frames:v 1 \
    -c:v libwebp -quality 88 \
    "$ROOT/website/assets/doom-easter-egg-poster.webp"

echo "==> done"
ffprobe -v error -show_entries stream=width,height:format=duration -of default=nw=1 \
    "$ROOT/docs/media/doom-easter-egg.mp4"
ls -la "$ROOT/docs/media/doom-easter-egg.mp4" \
    "$ROOT/website/assets/doom-easter-egg-poster.webp"
# The mp4 only reaches website/assets/ in CI (pages.yml), and that path is
# gitignored — so a local `npm run dev:website` preview 404s without this copy.
echo
echo "for a local preview: cp docs/media/doom-easter-egg.mp4 website/assets/"
# If the overlay's intrinsic size in website/js/main.js no longer matches, the
# modal reflows when the first frame decodes.
echo "if the dimensions above changed, update doomVideo.width/height in website/js/main.js"
