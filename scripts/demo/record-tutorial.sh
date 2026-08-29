#!/usr/bin/env bash
# Regenerate the onboarding tutorial's screenshots (media/tutorial/*.png).
#
# One PNG per step of docs/TUTORIAL.md, taken from the REAL TUI: this drives
# `Ctrl+N` through the creation flow the way a first-time reader does, so a step
# that stops looking like its screenshot breaks this script rather than quietly
# ageing the doc.
#
# It differs from scripts/demo/record.sh in three ways that matter:
#
#   * The profile starts with ZERO sessions and an EMPTY repo memory. The
#     tutorial's subject is the first launch, and a seeded list would show a
#     screen the reader cannot have yet.
#   * agents.toml is left for thurbox to seed, so the agent step shows the
#     built-in registry a fresh install really has. (Only when `claude` — the
#     seeded default — is installed; otherwise two stub agents are registered, so
#     the agent step is still reached rather than skipped.)
#   * It is NOT a VHS tape. VHS drives a TUI through ttyd and a headless browser;
#     stills need neither. thurbox runs in a detached tmux session, `tmux
#     send-keys` presses the keys, and each screenshot is `capture-pane -e`
#     rasterised by agg — the same asciicast→agg→ffmpeg chain
#     scripts/demo/record-doom.sh uses, minus the recording step. The consequence
#     worth knowing is that the REAL chords are pressed: VHS can emit neither
#     `Ctrl+/` (search) nor an F-key, so the tapes rebind them and the recordings
#     are of an interface nobody runs. Here `Ctrl+/` is sent as its actual byte.
#
# Isolation is the shared dev-sandbox helper, in its full flavor: a throwaway
# HOME/XDG/TMUX_TMPDIR, so this can touch neither your thurbox profile, nor your
# tmux server, nor your agent credentials.
#
# Requirements: cargo, git, tmux, agg, ffmpeg, python3, jq, sqlite3. A coding
# agent CLI is optional (see agents.toml above). agg ships no font, so FONT_DIR
# is provisioned under target/fonts: JetBrains Mono (whose box-drawing glyphs
# tile, which is what keeps a pane border from rendering as a dashed line), plus
# Noto Sans Symbols 2 for `⑂` (U+2442), the mark the session list puts on a
# worktree session. Both are fetched best-effort and fall back to a system
# DejaVu — a missing glyph is worth a box on screen, not a failed re-record.
#
# Usage:  scripts/demo/record-tutorial.sh [OUTPUT_DIR]
#
#   OUTPUT_DIR  where the PNGs go (default: <repo>/media/tutorial)
#
# Env: THEME (default doom), COLS, ROWS, FONT_SIZE, LINE_HEIGHT, FONT_DIR,
# FONT_FAMILY.
set -euo pipefail

log() { printf '[tutorial] %s\n' "$*" >&2; }
die() { printf '[tutorial] FATAL: %s\n' "$*" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="${1:-$REPO_ROOT/media/tutorial}"
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

# The theme every clip in scripts/demo/record.sh is recorded in, so the tutorial
# and the demo videos show the same thurbox.
THEME="${THEME:-doom}"
# 140x38 at font size 18 lands around 1500x970 — comfortably past the 80-column
# two-panel threshold in ui/layout.lua, and under the ~1568px long edge above
# which a reader's viewer resamples the PNG and the fine detail (status glyphs,
# tree marks) blurs.
COLS="${COLS:-140}"
ROWS="${ROWS:-38}"
FONT_SIZE="${FONT_SIZE:-18}"
# 1.2, not agg's 1.4 default: a line much taller than the glyph leaves a gap
# between one row's `│` and the next, and every pane border then reads as a
# dashed line. 1.0 closes the gap but clips descenders, so this is the pair of
# constraints met rather than either one won.
LINE_HEIGHT="${LINE_HEIGHT:-1.2}"
FONT_DIR="${FONT_DIR:-$REPO_ROOT/target/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono,Noto Sans Symbols 2}"
# The recording server's socket. Its own name, and inside the sandbox's private
# TMUX_TMPDIR, so it can reach neither your tmux nor thurbox's own `thurbox-dev`
# server.
SOCK="thurbox-tutorial-rec"

for bin in cargo git tmux agg ffmpeg python3 jq sqlite3; do
    command -v "$bin" >/dev/null 2>&1 || die "$bin not found on PATH"
done

# --- Fonts ------------------------------------------------------------------
# agg matches by family name and falls back per glyph, so the pair only has to
# exist somewhere in FONT_DIR. Every step is best-effort: this is a rendering
# nicety, not a reason to fail a re-record on a machine with no network.

# fetch_font <filename> <url> — cached, so a second run downloads nothing.
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

# --- Build the dev binaries (BEFORE the HOME override, so cargo finds ~/.cargo) -
log "Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli >&2
THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
[ -x "$THURBOX_BIN" ] || die "dev binary not found at $THURBOX_BIN"

HAS_CLAUDE=0
command -v claude >/dev/null 2>&1 && HAS_CLAUDE=1

# --- Isolated environment ----------------------------------------------------
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh

# The repo picker prints the ABSOLUTE path of a remembered repository, so the
# sandbox's `mktemp` home would put `/tmp/thurbox-sandbox.4tGnQx/home/code/…` in
# the screenshot a reader is meant to recognise their own `~/code` in. HOME is
# therefore a short, stable symlink to it: the isolation is unchanged (it points
# inside the sandbox, and teardown still removes the root), and the tilde
# expansion the flow does is still the real one.
HOME_LINK="${HOME_LINK:-/tmp/tutorial-home}"
if [ -e "$HOME_LINK" ] && [ ! -L "$HOME_LINK" ]; then
    die "$HOME_LINK exists and is not a symlink — refusing to touch it"
fi
ln -sfn "$HOME" "$HOME_LINK"
export HOME="$HOME_LINK"
# Same reasoning one level down: the worktree a session runs in is printed by
# `thurbox-cli session list`, and it is built under XDG_DATA_HOME. Pointing the
# XDG roots at their DEFAULT places inside the (now short) home keeps every path
# in a screenshot the shape a reader's own would be, and stays inside the
# sandbox — these resolve through the symlink to the sandbox root.
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

# Derived AFTER the overrides above, or the theme and the tripwire below would be
# written to and read from a profile the TUI never opens — which is a run of
# screenshots in the wrong theme, with nothing failing.
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"
DB_FILE="$XDG_DATA_HOME/thurbox-dev/thurbox.db"
mkdir -p "$CFG_DIR" "$(dirname "$DB_FILE")"

cleanup() {
    tmux -L "$SOCK" kill-server >/dev/null 2>&1 || true
    if [ -L "$HOME_LINK" ]; then rm -f "$HOME_LINK"; fi
    tbx_sandbox_teardown
}
trap cleanup EXIT INT TERM

# Hide `wsl.exe`: WSL distros are auto-discovered, and a discovered host puts the
# "Run on" step in front of the repo step — which would shift every keypress
# below by one. record.sh drops the same interop dirs for the same reason.
PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -v '^/mnt/[a-z]/' | paste -sd: -)
export PATH

# --- The repositories the tutorial browses to -------------------------------
# ~/code is what gets typed into the path field, so these have to exist in the
# throwaway HOME. `scratch` is deliberately NOT a git repo: it is what makes the
# browse dropdown's `●git` marker mean something on screen.
CODE_DIR="$HOME/code"
mkdir -p "$CODE_DIR/scratch"
for repo in api-server web-app; do
    dir="$CODE_DIR/$repo"
    mkdir -p "$dir/src"
    printf '# %s\n\nPart of the tutorial workspace.\n' "$repo" > "$dir/README.md"
    printf 'fn main() {\n    println!("%s");\n}\n' "$repo" > "$dir/src/main.rs"
    # `-b main` rather than the host's init.defaultBranch: the base-branch step
    # is a screenshot, and it should show what a reader's own repository most
    # likely has.
    git init -q -b main "$dir"
    git -C "$dir" -c user.email=tutorial@thurbox -c user.name=tutorial add -A
    git -C "$dir" -c user.email=tutorial@thurbox -c user.name=tutorial \
        commit -q -m "chore: init $repo"
done

# --- Agent registry ----------------------------------------------------------
if [ "$HAS_CLAUDE" -eq 0 ]; then
    log "claude not installed — registering stub agents so the flow still renders"
    cat > "$CFG_DIR/agents.toml" <<'AGENTS'
default = "shell"

[[agents]]
name = "shell"
command = "sh"
args = ["-c", "echo 'tutorial stub agent'; exec sh"]

[[agents]]
name = "shell-2"
command = "sh"
args = ["-c", "echo 'tutorial stub agent'; exec sh"]
AGENTS
fi

# --- Keep the agent's own first-run chrome off the screenshots ---------------
# A fresh HOME means a fresh agent CLI: update banners and trust dialogs would
# render instead of the agent, and some are modal enough to swallow a keypress.
# Only trust + onboarding flags are seeded, never credentials or history: claude
# is featured LOGGED OUT on purpose, because its welcome box prints the account's
# organisation name (auto-named after your email) when it is not.
export OPENCODE_DISABLE_AUTOUPDATE=true
export CODEX_DISABLE_UPDATE_CHECK=1
export npm_config_update_notifier=false
export NO_UPDATE_NOTIFIER=1

# The session created below runs in a WORKTREE, which is a directory the agent
# has never seen either — and an untrusted one puts a dialog over the headline
# screenshot. Its path is deterministic
# (`<data>/worktrees/<fnv1a(repo path)>/<branch>`, git::worktree::worktree_path),
# so it can be trusted before it exists.
TUTORIAL_BRANCH="rate-limit"
REPO_HASH=$(python3 - "$CODE_DIR/api-server" <<'PYHASH'
import sys

# FNV-1a 64, matching git::worktree::stable_repo_hash. Deliberately a fixed
# algorithm there, which is what makes predicting the path here legitimate.
h = 0xCBF29CE484222325
for byte in sys.argv[1].encode():
    h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
print(f"{h:016x}")
PYHASH
)
WORKTREE_DIR="$XDG_DATA_HOME/thurbox-dev/worktrees/$REPO_HASH/$TUTORIAL_BRANCH"

# Each path twice: as thurbox spells it, and resolved. HOME is a symlink (above),
# and an agent that canonicalises its cwd would not find the trust entry filed
# under the pretty spelling — which is the trust dialog back over the headline
# screenshot.
trusted_paths=()
for path in "$CODE_DIR/api-server" "$CODE_DIR/web-app" "$WORKTREE_DIR"; do
    # -m, not -f: the worktree does not exist yet, and -f fails on a path whose
    # parent directories are still missing.
    trusted_paths+=("$path" "$(readlink -m "$path")")
done

mkdir -p "$HOME/.local/bin"
jq -n '{hasCompletedOnboarding: true,
        projects: ($ARGS.positional
                   | map({(.): {hasTrustDialogAccepted: true}}) | add)}' \
    --args "${trusted_paths[@]}" > "$HOME/.claude.json"
if [ "$HAS_CLAUDE" -eq 1 ]; then
    # The symlink keeps claude's self-install check ("claude command missing")
    # quiet under the throwaway HOME.
    ln -sf "$(readlink -f "$(command -v claude)")" "$HOME/.local/bin/claude"
fi

# --- Profile state, before the first frame ----------------------------------
# The hooks extension is auto-activated and patches claude with `--settings`,
# which makes it ask "4 hooks are new or changed" on first launch in a fresh HOME
# — a modal over the agent pane in the tutorial's headline screenshot. The status
# dots it drives are not what these stills are about.
"$CLI_BIN" extension deactivate hooks >/dev/null 2>&1 || true
# The first-launch interface prompt is for somebody upgrading from v1; it would
# otherwise be the first thing every screenshot showed.
"$CLI_BIN" config accept-interface >/dev/null 2>&1 || true
# Touch the database so the theme can be persisted into it.
"$CLI_BIN" session list >/dev/null 2>&1 || true

sqlite3 "$DB_FILE" \
    "INSERT INTO metadata (key, value) VALUES ('active_theme', '$THEME') \
     ON CONFLICT(key) DO UPDATE SET value = excluded.value" \
    || log "warning: could not set the theme (continuing with the default)"

# Tripwire: this profile must be empty. A session here means the isolation broke
# and the dev binary is reading a real database — the tutorial's first screenshot
# would then be of somebody's actual work.
sessions=$(sqlite3 "$DB_FILE" \
    "SELECT count(*) FROM sessions WHERE deleted_at IS NULL" 2>/dev/null || echo 0)
[ "$sessions" = "0" ] || die "sandbox is not empty ($sessions session(s)) — \
THURBOX_CONFIG_DIR/THURBOX_DATA_DIR may point at your real data. Aborting."

# --- The recording terminal --------------------------------------------------
# `status off` because the pane has to own every row it was given, and
# `escape-time 0` so an Escape below is not held back waiting for a sequence that
# never comes. RGB is declared for the pane's own TERM: capture-pane -e re-emits
# the colours tmux stored, and a downgraded palette here is a downgraded palette
# in every PNG.
TMUX_CONF="$TBX_SANDBOX_ROOT/tmux.conf"
cat > "$TMUX_CONF" <<'TMUXCONF'
set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*:Tc"
set -g status off
set -g escape-time 0
TMUXCONF

tmux -L "$SOCK" -f "$TMUX_CONF" new-session -d -s tut -x "$COLS" -y "$ROWS" \
    "$THURBOX_BIN"

# The pane every helper below acts on. The run drives two in turn: the TUI, then
# a shell for the `thurbox-cli` still, once the TUI has quit and taken its window
# with it.
TARGET=tut

send()  { tmux -L "$SOCK" send-keys -t "$TARGET" "$@"; }
write() { tmux -L "$SOCK" send-keys -t "$TARGET" -l -- "$1"; }
# Ctrl+/ is 0x1F, which has no tmux key name — the byte is sent directly. (This
# is the chord VHS cannot emit at all, which is why the tapes rebind search.)
search() { tmux -L "$SOCK" send-keys -t "$TARGET" -H 1f; }
# run <command> <settle seconds> — type a command line and let its output land.
run()   { write "$1"; send Enter; sleep "$2"; }

SHOT_INDEX=0
# shot <name> — rasterise what the pane is showing right now.
#
# capture-pane -e re-emits the cell colours as SGR, which is the whole frame in a
# form agg can replay: a one-event asciicast is a screenshot in cast clothing.
# The cursor is not part of a capture, so it is carried separately — without it
# the path field and the name field show their text with no caret, which is the
# one thing a reader has to see to know where typing lands.
shot() {
    local name="$1"
    SHOT_INDEX=$((SHOT_INDEX + 1))
    local stem
    stem=$(printf '%02d-%s' "$SHOT_INDEX" "$name")
    local work="$TBX_SANDBOX_ROOT/shots"
    mkdir -p "$work"

    tmux -L "$SOCK" capture-pane -p -e -N -t "$TARGET" > "$work/$stem.ansi"
    local geom
    geom=$(tmux -L "$SOCK" display -p -t "$TARGET" \
        '#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{cursor_flag}')

    # shellcheck disable=SC2086 # $geom is five fields, split on purpose
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

    # `fontdue` rather than the default resvg renderer: resvg parses the family
    # list as CSS and chokes on a name ending in a digit ("Noto Sans Symbols 2"),
    # falling back to Times New Roman for the whole frame — a blank-looking PNG.
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

# thurbox boots, resolves its interface and paints the first frame.
sleep 6
# 01 — first launch: an empty session list beside an empty centre pane.
shot first-launch

# 02 — Ctrl+N opens the creation flow on the repo step. Memory is empty, so the
# only row is the interface directory the kernel offers on its own account.
send C-n; sleep 2
shot repo-picker

# 03 — tab moves focus to the path field. The trailing slash is what makes the
# next tab browse rather than complete: a completion needs a prefix to extend.
send Tab; sleep 1
# shellcheck disable=SC2088 # a literal `~` is what gets typed; the kernel expands it
write "~/code/"; sleep 2
shot add-repo-path

# 04 — tab again: the browse dropdown lists that directory, marking which
# subdirectories are git repositories.
send Tab; sleep 3
shot browse-directory

# 05 — enter on a git row adds it to repo memory, selects it, and puts focus back
# on the list with the cursor on the row just added.
send Enter; sleep 3
shot repo-added

# 06 — `w` gives the selected repository its own worktree.
write "w"; sleep 2
shot worktree-mode

# 07 — enter leaves the repo step. Worktree mode is what makes the base-branch
# step appear at all.
send Enter; sleep 3
shot base-branch

# 08 — the session name. The placeholder is the suggestion `enter` would take.
send Enter; sleep 2
write "$TUTORIAL_BRANCH"; sleep 2
shot session-name

# 09 — the branch name, prefilled from the session name.
send Enter; sleep 2
shot branch-name

# 10 — the agent. Skipped when only one agent is defined; a fresh install seeds
# the built-in registry, so it is not.
send Enter; sleep 2
shot agent-picker

# 11 — created: the worktree, the tmux window and the agent, with the session
# selected in the list.
send Enter; sleep 12
shot session-running

# 12 — the second session is cheaper than the first: the repository is in memory
# now, so the picker opens with it already listed and `space` is all it takes.
send C-n; sleep 3
shot repo-remembered
send Escape; sleep 1

# Out of the agent terminal for the rest of the run. Focus-cycling is never
# passed through to the agent, which is what makes it the way out.
send C-h; sleep 1

# 13 — search: matches highlight inside the panes being searched rather than
# being reprinted in the strip.
search; sleep 1
write "rate"; sleep 2
shot search
send Escape; sleep 1

# 14 — the key list, rendered from the live registry.
send C-g; sleep 2
shot keybindings
send Escape; sleep 1

# Quit: detaches, leaving every agent running under the sandbox's tmux.
send C-q; sleep 3

# 15 — the other half of the tutorial: the same sessions from `thurbox-cli`,
# which is what an agent inside a session reaches for. Its own tmux session
# because the TUI's window closed with the TUI.
tmux -L "$SOCK" -f "$TMUX_CONF" new-session -d -s cli -x "$COLS" -y "$ROWS" \
    "bash --norc"
TARGET=cli
sleep 1
# A bare `$` reads as a prompt in a doc; `bash-5.2$` reads as this machine.
run "PS1='\$ '; clear" 1
run "thurbox-cli session list" 3
# A session created with no TUI running at all: it appears in the list, and in
# the TUI within a tick, because both binaries share one database.
run "thurbox-cli session create --name docs --repo-path ~/code/web-app" 6
run "thurbox-cli session list" 3
shot cli

count=$(find "$OUT_DIR" -maxdepth 1 -name '*.png' | wc -l | tr -d ' ')
log "Captured $count screenshot(s) in $OUT_DIR"
[ "$count" -eq 0 ] && die "no screenshots produced"
printf '%s\n' "$OUT_DIR"
