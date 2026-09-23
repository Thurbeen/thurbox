#!/usr/bin/env bash
# Record focus moving between panes, and optionally a still of each focus state
# under several themes.
#
#   scripts/demo/record-focus.sh [output.gif]      # default: media/focus.gif
#   STILLS=<dir> THEMES="default github-light" scripts/demo/record-focus.sh
#
# The demo moves focus the way a user does — ^H to the session list, ^L back to
# the terminal, the search strip opened and closed — in the shipped `default`
# preset, pinned below (DEMO_THEME) rather than inherited.
#
# With STILLS set it records no GIF. Instead, for every theme in THEMES, it
# boots a fresh interface and saves one PNG per focus state (sessions, terminal,
# search), plus a `-mono` copy of each with every colour stripped and only the
# attributes left (bold, reverse). The mono copy is the accessibility check: it
# is what a monochrome terminal or a reader who cannot tell the two colours
# apart is left with, so a focus cue that is only a colour vanishes from it.
#
# Like record-search.sh this is asciinema + agg driven through tmux, the agent
# is a filler stand-in declared in the sandbox's own agents.toml, and the
# sandbox is fully hermetic (tbx_sandbox_init_full): its own HOME, XDG dirs and
# tmux socket, all removed on exit.
#
# Needs: a built thurbox + thurbox-cli (target/debug, `just build`), tmux, git,
# sqlite3, python3, asciinema 2.x and agg on PATH (and ffmpeg for STILLS).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-$ROOT/media/focus.gif}"
COLS="${COLS:-140}"
ROWS="${ROWS:-36}"
STILLS="${STILLS:-}"
DEMO_THEME="${DEMO_THEME:-default}"
THEMES="${THEMES:-$DEMO_THEME}"
AGG_THEME="${AGG_THEME:-asciinema}"
# A light preset is used on a light terminal, so its stills are rendered on one.
AGG_LIGHT_THEME="${AGG_LIGHT_THEME:-github-light}"
FONT_DIR="${FONT_DIR:-/usr/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono}"

missing=
tools="asciinema agg tmux git sqlite3 python3"
[ -n "$STILLS" ] && tools="$tools ffmpeg"
for tool in $tools; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
for bin in thurbox thurbox-cli; do
    [ -x "$ROOT/target/debug/$bin" ] || missing="$missing target/debug/$bin"
done
[ -n "$missing" ] && { echo "missing:$missing (run: just build)" >&2; exit 2; }
[ -n "$STILLS" ] && STILLS="$(mkdir -p "$STILLS" && cd "$STILLS" && pwd)"

export TBX_REPO_ROOT="$ROOT"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh
S="$TBX_SANDBOX_ROOT"
TM="tmux -L thurbox-focus-demo"
cleanup() {
    $TM kill-server 2>/dev/null || true
    tbx_sandbox_teardown
}
trap cleanup EXIT INT TERM

# --- the world: one repository, three sessions running the stand-in agent ----
CONFIG="$XDG_CONFIG_HOME/thurbox-dev"
mkdir -p "$CONFIG" "$S/bin"
printf '[features]\nautomations = false\nversion_check = false\nauto_update = false\n' \
    >"$CONFIG/settings.toml"

cat >"$S/bin/demo-agent" <<'AGENT'
#!/bin/sh
# A stand-in coding agent: a few lines of filler once thurbox has sized the
# pane (text printed at the window's birth width re-wraps into spliced rows),
# then a prompt that waits.
birth=$(stty size 2>/dev/null)
waited=0
while [ "$(stty size 2>/dev/null)" = "$birth" ] && [ "$waited" -lt 150 ]; do
    sleep 0.1
    waited=$((waited + 1))
done
sleep 0.3
clear
printf '● Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n'
printf '  Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n'
printf '  Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.\n\n'
printf '● Duis aute irure dolor in reprehenderit in voluptate velit esse.\n\n'
printf 'ready.\n\n› '
while IFS= read -r _; do printf '● Done.\n\n› '; done
AGENT
chmod +x "$S/bin/demo-agent"
printf 'default = "demo"\n\n[[agents]]\nname = "demo"\ncommand = "%s"\nargs = []\n' \
    "$S/bin/demo-agent" >"$CONFIG/agents.toml"

REPO="$S/checkout"
mkdir -p "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" -c user.name=demo -c user.email=demo@example.invalid \
    commit -q --allow-empty -m init
for name in login-fix api-refactor docs-pass; do
    thurbox-cli session create --name "$name" --repo-path "$REPO" --agent demo >/dev/null
done
thurbox-cli config accept-interface >/dev/null

DB="$XDG_DATA_HOME/thurbox-dev/thurbox.db"
# Pin a theme, then read it back through thurbox itself so a schema change fails
# here rather than recording the fallback silently.
pin_theme() {
    sqlite3 "$DB" "INSERT INTO metadata (key, value) VALUES ('active_theme', '$1') \
        ON CONFLICT(key) DO UPDATE SET value = excluded.value"
    thurbox-cli config show --json | grep -q "\"theme\": *\"$1\"" || {
        echo "the theme did not take: expected $1" >&2
        exit 1
    }
}

cat >"$S/run.sh" <<RUN
#!/usr/bin/env bash
cd "$S"
exec thurbox
RUN
chmod +x "$S/run.sh"

k() { $TM send-keys -t 0 "$@"; }
wait_for() {
    for _ in $(seq 1 60); do
        $TM capture-pane -p -t 0 2>/dev/null | grep -qF "$1" && return 0
        sleep 0.5
    done
    echo "timed out waiting for: $1" >&2
    $TM capture-pane -p -t 0 >&2
    exit 1
}

# A PNG of what the pane holds now: the screen as tmux stores it (colours and
# attributes included) wrapped in a one-event cast, rendered by agg, then the
# last frame taken out of the GIF. `mono` drops every colour parameter from the
# SGR sequences and keeps the rest.
still() { # still <name> <mono:0|1> <thurbox theme>
    local ansi="$S/still.ansi" palette="$AGG_THEME"
    case "$3" in *light* | *latte* | *day* | *dawn*) palette="$AGG_LIGHT_THEME" ;; esac
    $TM capture-pane -p -e -t 0 >"$ansi"
    python3 - "$ansi" "$COLS" "$ROWS" "$2" >"$S/still.cast" <<'CAST'
import json, re, sys

path, cols, rows, mono = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4] == "1"
text = open(path, encoding="utf-8", errors="replace").read().rstrip("\n")


def strip(match):
    params = match.group(1).split(";") if match.group(1) else ["0"]
    kept, i = [], 0
    while i < len(params):
        p = params[i]
        if p in ("38", "48", "58"):
            # 38;5;n or 38;2;r;g;b
            i += 3 if i + 1 < len(params) and params[i + 1] == "5" else 5
            continue
        n = int(p) if p.isdigit() else 0
        if not (30 <= n <= 39 or 40 <= n <= 49 or 90 <= n <= 97 or 100 <= n <= 107):
            kept.append(p)
        i += 1
    return "\x1b[" + ";".join(kept) + "m" if kept else ""


if mono:
    text = re.sub(r"\x1b\[([0-9;]*)m", strip, text)
screen = "\x1b[?25l\x1b[H\x1b[2J" + text.replace("\n", "\r\n")
print(json.dumps({"version": 2, "width": cols, "height": rows}))
print(json.dumps([0.0, "o", screen]))
print(json.dumps([0.5, "o", ""]))
CAST
    agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size 14 \
        --theme "$palette" "$S/still.cast" "$S/still.gif" >/dev/null 2>&1
    ffmpeg -loglevel error -y -i "$S/still.gif" -frames:v 1 -update 1 "$STILLS/$1.png"
}
stills() { # stills <theme> <state>
    still "$1-$2" 0 "$1"
    still "$1-$2-mono" 1 "$1"
}

if [ -n "$STILLS" ]; then
    for theme in $THEMES; do
        pin_theme "$theme"
        $TM new-session -d -x "$COLS" -y "$ROWS" "$S/run.sh"
        wait_for "ready."
        sleep 1.5
        # Boot focus is the terminal; ^H hands it to the session list.
        stills "$theme" 1-terminal
        k C-h
        sleep 1
        stills "$theme" 2-sessions
        k C-_
        sleep 1.2
        stills "$theme" 3-search
        k Escape
        sleep 0.5
        k C-q
        sleep 1.5
        $TM kill-server 2>/dev/null || true
    done
    ls "$STILLS"
    exit 0
fi

# --- record -------------------------------------------------------------------
pin_theme "$DEMO_THEME"
CAST="$S/focus.cast"
$TM new-session -d -x "$COLS" -y "$ROWS" \
    "asciinema rec --overwrite --quiet --command '$S/run.sh' '$CAST'"

wait_for "ready."
sleep 2
# Terminal → session list → down a row → back to the terminal it opened.
k C-h
sleep 1.8
k Down
sleep 1.2
k Down
sleep 1.2
k Enter
sleep 2
k C-h
sleep 1.8
k C-l
sleep 1.8
# The search strip takes focus while it is open; Esc closes it and the ring
# settles on its first pane, the session list.
k C-_
sleep 2
k Escape
sleep 2

k C-q
for _ in $(seq 1 20); do [ -s "$CAST" ] && break; sleep 0.5; done
sleep 1

# Cut the cast where teardown starts, so the GIF loops on the last frame and not
# a bare shell: the first cursor-show or alternate-screen exit after the last
# cursor-hide.
python3 - "$CAST" <<'TRIM'
import json, sys

path = sys.argv[1]
lines = open(path).read().splitlines()
events = [json.loads(line)[2] for line in lines[1:]]
hidden = max((i for i, data in enumerate(events) if "\x1b[?25l" in data), default=0)
for i in range(hidden + 1, len(events)):
    if "\x1b[?25h" in events[i] or "\x1b[?1049l" in events[i]:
        open(path, "w").write("\n".join(lines[: i + 1]) + "\n")
        break
TRIM

mkdir -p "$(dirname "$OUT")"
agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size 14 \
    --idle-time-limit 2 --theme "$AGG_THEME" "$CAST" "$OUT"
ls -lh "$OUT"
