#!/usr/bin/env bash
# Regenerate the website's `iddqd` easter-egg clip: Doom running *inside* a
# thurbox pane.
#
#   docs/media/doom-easter-egg.mp4              (copied into website/assets/ at
#                                                deploy time by pages.yml)
#   website/assets/doom-easter-egg-poster.webp  (committed; the poster frame)
#
# **Doom is a plugin now, not an agent.** This used to be recorded through the
# `pi` CLI and its `pi-doom` extension — a game pretending to be a coding agent
# to borrow the one field of the session model that spawns a pty. thurbox-doom
# asks the kernel for a *program pane* of its own (`Capability::Program`), so the
# recording needs no agent to install and no transcript tidied up on screen:
# install the plugin, grant it the capability, press `f7`.
#
#   https://github.com/Thurbeen/thurbox-doom
#
# Two consequences for the script below. The plugin arrives by **clone** (its WAD
# is binary, and the file-by-file fetch path decodes what it fetches as UTF-8),
# so recording needs the network once. And a `program` grant is a **decision made
# in a running interface** — there is deliberately no `thurbox-cli` for it — so the
# sandbox seeds the grant the settings modal would have written.
#
# Unlike the other demos this is not a VHS tape. VHS drives a TUI through
# ttyd + a headless browser; here the whole point is a *nested* TUI (thurbox
# rendering Doom), and the toolchain is lighter: asciinema records the real
# thurbox to an asciicast and agg rasterises it to frames. No browser.
#
# The clip is Doom's own attract demo, so nothing has to be played. The engine
# does handle held keys (it infers releases from auto-repeat timing, which is how
# it works around thurbox forwarding key *presses* but not *releases*), but
# scripting a level through `tmux send-keys` is a recording nobody can re-run and
# get the same footage from — the attract demo is the same every time.
#
# Requirements:
#   thurbox + thurbox-cli on PATH   (the binaries under test)
#   asciinema **2.x**, agg, ffmpeg, ffprobe, tmux, git, node, python3 (>= 3.11),
#   sqlite3. The 2.x pin is load-bearing: asciinema 3 records asciicast v3, an
#   interval-based format trim-cast.mjs does not parse — it reads v2's absolute
#   timestamps. `uv tool install 'asciinema==2.4.0'` is one way to get it.
#   network access, once, for the plugin clone (or point DOOM_PLUGIN_SRC at a
#   local checkout)
#   optionally any of claude / codex / opencode / agy — one session each, so the
#   session list beside Doom is not empty. Missing ones are skipped; their panes
#   never appear in the kept window, which starts long after `f7`.
#   a font directory for agg, which ships none. Point FONT_DIR at it. A Nerd Font
#   covers everything on its own
#   (https://github.com/ryanoasis/nerd-fonts/releases -> JetBrainsMono.tar.xz);
#   otherwise the coverage to check is the session list's, not Doom's — the engine
#   only ever paints `▀`, while the rows carry the worktree mark `⑂` (U+2442) and
#   the working spinner's braille (U+2800..), which plain JetBrains Mono does not
#   have. What this clip was recorded with, as a working pair:
#     FONT_DIR   a directory holding JetBrainsMono-Regular.ttf and
#                NotoSansSymbols2-Regular.ttf
#     FONT_FAMILY "JetBrains Mono,Noto Sans Symbols 2"
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

FONT_DIR="${FONT_DIR:-$HOME/.local/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrainsMono Nerd Font,DejaVuSansM Nerd Font}"
COLS="${COLS:-160}"
ROWS="${ROWS:-44}"
FONT_SIZE="${FONT_SIZE:-20}"
FPS="${FPS:-25}"
# The theme the clip is recorded in, persisted as metadata.active_theme (see
# src/session/theme_config.rs). `doom` for the obvious reason, and it is what
# scripts/demo/record.sh puts every other clip in.
THEME="${THEME:-doom}"
# Where the plugin comes from. A `git+` prefix is one of the three spellings that
# clone (`.git` suffix and `git@host:path` are the others); a filesystem path
# works too, which is how you record against a local checkout.
DOOM_PLUGIN_SRC="${DOOM_PLUGIN_SRC:-git+https://github.com/Thurbeen/thurbox-doom}"

# --- The recording's phases, in seconds since thurbox launched ---------------
# thurbox boots, adopts the seeded sessions and loads the plugin.
BOOT_SECS="${BOOT_SECS:-10}"
# How long Doom runs after `f7` before Ctrl+Q ends the recording.
DOOM_SECS="${DOOM_SECS:-60}"
# Window kept from the raw cast. Doom's attract sequence, timed from `f7`: the
# title screen for ~5 s, DEMO1 for ~15 s, then the id credits page — which is
# *static*, and a clip that lands on it is 5 seconds of a still image. DEMO2
# starts at ~27 s and runs well past 90, so the window is taken from there:
# the engine's boot jitters by a second or two between runs and inside DEMO2
# that cannot push the cut onto a page screen. The exact offset is worth an eye
# after a re-record: the clip loops, so opening on one of Doom's full-screen
# damage flashes means a red wash every 21 seconds.
START="${START:-$((BOOT_SECS + 37))}"
END="${END:-$((START + 21))}"
# Poster frame, in seconds into the *trimmed* clip. Worth re-picking after a
# re-record and worth looking at: Doom flashes the whole view red when the player
# takes damage and olive when it picks something up, and a poster that lands on
# one of those is a wash of colour rather than a room — which is all a
# reduced-motion visitor ever sees, since the overlay then shows the poster and
# waits to be asked to play.
POSTER_AT="${POSTER_AT:-3}"

SBX="${SBX:-/tmp/thurbox-doom-rec}"

# The sessions in the left column, named after the work rather than the agent —
# the same narrative scripts/demo/record.sh records the other clips against.
# Paired with whichever CLIs are installed, in order; extras are dropped.
SESSION_NAMES=(fix-osc52-tmux add-wsl-host-tests perf-session-order-cache docs-remote-hooks)
SESSION_BRANCHES=(fix/osc52-tmux test/wsl-hosts perf/session-order docs/remote-hooks)

for bin in thurbox thurbox-cli node python3 sqlite3 asciinema agg ffmpeg ffprobe tmux git; do
    command -v "$bin" >/dev/null 2>&1 || {
        echo "error: $bin not found on PATH" >&2
        exit 1
    }
done
# The trust seed reads plugins.lock, so the TOML parser has to be there. Checked
# up front rather than at the point of use, where it would fail after the clone.
python3 -c 'import tomllib' 2>/dev/null || {
    echo "error: python3 has no tomllib (needs >= 3.11)" >&2
    exit 1
}
[ -d "$FONT_DIR" ] || {
    echo "error: FONT_DIR '$FONT_DIR' does not exist (agg ships no font)" >&2
    exit 1
}
[ "$END" -lt $((BOOT_SECS + DOOM_SECS)) ] || {
    echo "error: END ($END) is past the end of the recording ($((BOOT_SECS + DOOM_SECS))s)" >&2
    exit 1
}

# Fully isolated: thurbox's own config/data plus a private TMUX_TMPDIR, so this
# never touches your real sessions or the release `thurbox` socket. Kept short —
# AF_UNIX socket paths are length-limited.
export TMUX_TMPDIR="$SBX/tmux"
export THURBOX_CONFIG_DIR="$SBX/config"
export THURBOX_DATA_DIR="$SBX/data"

rm -rf "$SBX"
mkdir -p "$SBX"/{tmux,config,data}

# Written before the first thurbox call, which is what seeds this file: both
# flags reach the network, and each one spoils a recording in its own way.
# `version_check` puts an `⬆ vX available` segment in the top band — the clip
# would advertise an upgrade and date itself. `auto_update` silently downloads
# and replaces the installed binaries on startup, which is not something a
# recording should do to the machine it is recording on.
cat > "$THURBOX_CONFIG_DIR/settings.toml" <<'SETTINGS'
[features]
version_check = false
auto_update = false
SETTINGS
# Named `thurbox` because the session list groups by repository and prints the
# directory's basename as the group header.
REPO="$SBX/thurbox"
# `-b main`, not git's default: a worktree is created off `main` unless told
# otherwise, and on a box whose init.defaultBranch is `master` every
# `session create --worktree-branch` below fails with `invalid reference: main`.
git init -q -b main "$REPO"
printf 'thurbox\n' > "$REPO/README.md"
git -C "$REPO" add -A
git -C "$REPO" -c user.email=demo@thurbox -c user.name=demo -c commit.gpgsign=false \
    commit -qm init

CAST="$SBX/doom.cast"
TRIMMED="$SBX/trimmed.cast"
GIF="$SBX/doom.gif"

# Both kills are scoped by the exported TMUX_TMPDIR above, which is why killing
# the `thurbox` socket by name here cannot reach your real thurbox server. It is
# also what reaps Doom, whose program pane is a window on that socket.
cleanup() {
    tmux -L thurbox-doom-rec kill-server 2>/dev/null
    tmux -L thurbox kill-server 2>/dev/null
}
trap cleanup EXIT

# Resolve the paths thurbox itself resolves, rather than assuming the layout: a
# dev build and a release build read different profiles, and both are plausible
# on PATH here.
paths=$(thurbox-cli config show --json 2>/dev/null | python3 -c '
import json, sys
paths = json.load(sys.stdin)["paths"]
print(paths["ui_dir"]); print(paths["ui_json"]); print(paths["database"])
')
[ -n "$paths" ] || {
    echo "error: could not read the resolved paths from thurbox-cli config show" >&2
    exit 1
}
{
    IFS= read -r UI_DIR
    IFS= read -r UI_JSON
    IFS= read -r DB
} <<< "$paths"

echo "==> installing the doom plugin into $UI_DIR"
thurbox-cli plugin install "$DOOM_PLUGIN_SRC" --text || exit 1

# The plugin ships one built engine (linux-x86_64) and names the platform it has
# nothing for rather than exec'ing a path it has no reason to believe in — which
# would be a recording of that panel instead of a recording of Doom. Same
# `<os>-<arch>` spelling `thurbox.platform` publishes (Rust's `env::consts`).
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
# A capability is granted in the settings modal (Interface tab, `t`) and nowhere
# else: from a CLI, trust would be a decision made about a running interface by
# something that is not it. So the sandbox writes the grant the modal writes —
# `{pin, digest}` for an installed file, keyed by absolute path (two interface
# directories must not share a grant), which is what `App::apply_trust` does.
# Both halves come out of plugins.lock rather than being recomputed: the digest
# recorded there is the one the delivered file has, so the row reads `trusted`
# and not `trusted · modified`.
python3 - "$UI_DIR" "$UI_JSON" <<'PYTRUST' || exit 1
import json, pathlib, sys, tomllib

ui_dir, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
lock = tomllib.loads((ui_dir / "plugins.lock").read_text())
entries = lock.get("plugin", [])
if len(entries) != 1:
    sys.exit(f"expected one installed plugin, found {len(entries)}")
entry = entries[0]
pane = entry["file"]
grant = {"pin": f"{entry['src']}@{entry['version']}", "digest": entry["files"][pane]}
out.write_text(json.dumps({"trusted": {str(ui_dir / pane): grant}}, indent=2))
print(f"trusting {pane} at {grant['pin']}")
PYTRUST

# --- The session list beside Doom: one session per installed agent CLI --------
# Real CLIs, as in scripts/demo/record.sh, because the rows are the real thing or
# they are a mock-up. No agent pane is ever in the kept window — that starts half a
# minute after `f7`, with Doom on screen throughout — so nothing an agent prints,
# account name or past conversation or a trust prompt, can reach the clip.
agents=()
for a in claude codex opencode agy; do
    command -v "$a" >/dev/null 2>&1 && agents+=("$a")
done
if [ ${#agents[@]} -eq 0 ]; then
    echo "warning: no agent CLI found — recording with an empty session list" >&2
else
    {
        echo "default = \"${agents[0]}\""
        for a in "${agents[@]}"; do
            printf '\n[[agents]]\nname = "%s"\ncommand = "%s"\n' "$a" "$a"
        done
    } > "$THURBOX_CONFIG_DIR/agents.toml"

    for i in "${!agents[@]}"; do
        [ "$i" -lt "${#SESSION_NAMES[@]}" ] || break
        echo "==> creating session ${SESSION_NAMES[$i]} (${agents[$i]})"
        thurbox-cli session create --name "${SESSION_NAMES[$i]}" \
            --agent "${agents[$i]}" --repo-path "$REPO" \
            --worktree-branch "${SESSION_BRANCHES[$i]}" --text 2>&1 | head -2
    done
fi

# The db and its metadata table exist by now (the calls above opened them), and
# no TUI is running yet, so these writes are conflict-free.
#
# `v2_interface_acknowledged` is the one that is not cosmetic: the v1 -> v2
# consent gate asks any profile with session history, and seeding sessions gives
# a fresh profile exactly that. Unacknowledged, thurbox stops on the notice and
# the recording is 60 seconds of a wall of text.
sqlite3 "$DB" "
INSERT INTO metadata (key, value) VALUES ('v2_interface_acknowledged', '1')
  ON CONFLICT(key) DO UPDATE SET value = excluded.value;
INSERT INTO metadata (key, value) VALUES ('active_theme', '$THEME')
  ON CONFLICT(key) DO UPDATE SET value = excluded.value;" || exit 1

echo "==> recording thurbox (${COLS}x${ROWS})"
# asciinema owns the pty, so `tmux send-keys` below reaches thurbox through it.
tmux -L thurbox-doom-rec new-session -d -x "$COLS" -y "$ROWS" -c "$REPO" -s r \
    "TMUX_TMPDIR=$TMUX_TMPDIR THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR \
     THURBOX_DATA_DIR=$THURBOX_DATA_DIR PATH=$PATH \
     asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS -c thurbox '$CAST'"

send() { tmux -L thurbox-doom-rec send-keys -t r "$@"; }

sleep "$BOOT_SECS"
# `f7` is the plugin's global chord, so it reaches a pane that is not on screen
# yet — Doom takes the `center` switch slot, behind the agent. The pane asks for
# its program on the first frame it draws, which is this one, so Doom starts
# here and the phase arithmetic above is timed from it.
send F7
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
