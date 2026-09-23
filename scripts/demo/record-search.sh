#!/usr/bin/env bash
# Record the full-content search demo: a prompt typed into a session, scrolled
# far out of view by the work that followed it, then found from the search strip
# and opened — landing on the line, scrolled back, not merely on the session.
#
#   scripts/demo/record-search.sh [output.gif]      # default: media/search-full-content.gif
#
# Like record-doom.sh this is asciinema + agg driven through tmux rather than a
# VHS tape: the recording presses the real chords a user presses (`ctrl+/` is
# not something VHS can type), and nothing needs a browser.
#
# The "agent" is a stand-in declared in the sandbox's own agents.toml — a shell
# loop that answers each prompt with a long lorem-ipsum transcript — because
# thurbox is agent-neutral and the point is the terminal's scrollback, not any
# one CLI. Filler, not real-looking output: the recording reads as a working
# session without showing anything real, and it is long enough (~200 lines a
# reply, on a ~36-row pane) that the prompt has genuinely scrolled away. The
# filler never contains the words searched for, so every hit is the prompt.
#
# The theme is pinned, not inherited: thurbox's shipped `default` preset
# (DEMO_THEME), written to metadata.active_theme the way scripts/demo/record.sh
# pins its own. That preset draws in the terminal's ANSI colours on its native
# background, so agg's terminal palette is pinned too (AGG_THEME), or a re-run
# on another machine would render the same frames differently.
#
# Fully hermetic (tbx_sandbox_init_full): its own HOME, XDG dirs and tmux
# socket, all removed on exit.
#
# Needs: a built thurbox + thurbox-cli (target/debug, `just build`), tmux,
# git, asciinema 2.x and agg on PATH. FONT_DIR/FONT_FAMILY pass through to agg.
# SNAP=<dir> saves what the screen held at each step, which is how a missed key
# is told apart from a key that landed somewhere unexpected.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-$ROOT/media/search-full-content.gif}"
COLS="${COLS:-140}"
ROWS="${ROWS:-40}"
SNAP="${SNAP:-}"
DEMO_THEME="${DEMO_THEME:-default}"
AGG_THEME="${AGG_THEME:-asciinema}"
FONT_DIR="${FONT_DIR:-/usr/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono}"

missing=
for tool in asciinema agg tmux git sqlite3; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
for bin in thurbox thurbox-cli; do
    [ -x "$ROOT/target/debug/$bin" ] || missing="$missing target/debug/$bin"
done
[ -n "$missing" ] && { echo "missing:$missing (run: just build)" >&2; exit 2; }

export TBX_REPO_ROOT="$ROOT"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh
S="$TBX_SANDBOX_ROOT"
TM="tmux -L thurbox-search-demo"
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
# A stand-in coding agent that talks in lorem ipsum: a short transcript on
# start, then a long "reply" to every prompt. Deterministic, so every recording
# scrolls the same distance.
filler() { # filler <paragraphs> <seed>
    awk -v n="$1" -v seed="$2" 'BEGIN {
        split("Lorem ipsum dolor sit amet, consectetur adipiscing elit.|" \
              "Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.|" \
              "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.|" \
              "Duis aute irure dolor in reprehenderit in voluptate velit esse.|" \
              "Excepteur sint occaecat cupidatat non proident, sunt in culpa.|" \
              "Curabitur pretium tincidunt lacus, nulla gravida orci a odio.|" \
              "Nullam varius, turpis et commodo pharetra, est eros bibendum elit.|" \
              "Praesent dapibus, neque id cursus faucibus, tortor neque egestas.|" \
              "Vestibulum tortor quam, feugiat vitae, ultricies eget, tempor sit.|" \
              "Aenean ultricies mi vitae est, mauris placerat eleifend leo.|" \
              "Quisque sit amet est et sapien ullamcorper pharetra.|" \
              "Donec non enim in turpis pulvinar facilisis, ut felis.", s, "|")
        k = seed
        for (p = 1; p <= n; p++) {
            printf "● %s\n", s[k % 12 + 1]; k += 5
            for (l = 0; l < 4 + k % 3; l++) {
                printf "  %s\n", s[k % 12 + 1]; k += 7
            }
            print ""
        }
    }'
}
# Speak only once thurbox has attached and sized the pane: text printed at the
# window's birth width is re-wrapped on the resize and reads as spliced rows.
birth=$(stty size 2>/dev/null)
waited=0
while [ "$(stty size 2>/dev/null)" = "$birth" ] && [ "$waited" -lt 150 ]; do
    sleep 0.1
    waited=$((waited + 1))
done
sleep 0.3
clear
filler 6 1
printf 'ready.\n'
turn=0
while printf '\n› ' && IFS= read -r prompt; do
    turn=$((turn + 1))
    printf '\n'
    filler 40 "$turn"
    printf '● Done.\n'
done
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

# Pin the theme (see the header), then read it back through thurbox itself so a
# schema change fails here rather than recording the fallback silently.
DB="$XDG_DATA_HOME/thurbox-dev/thurbox.db"
sqlite3 "$DB" "INSERT INTO metadata (key, value) VALUES ('active_theme', '$DEMO_THEME') \
    ON CONFLICT(key) DO UPDATE SET value = excluded.value"
thurbox-cli config show --json | grep -q "\"theme\": *\"$DEMO_THEME\"" || {
    echo "the theme did not take: expected $DEMO_THEME" >&2
    exit 1
}

# --- record -------------------------------------------------------------------
CAST="$S/search.cast"
cat >"$S/run.sh" <<RUN
#!/usr/bin/env bash
cd "$S"
exec thurbox
RUN
chmod +x "$S/run.sh"
$TM new-session -d -x "$COLS" -y "$ROWS" \
    "asciinema rec --overwrite --quiet --command '$S/run.sh' '$CAST'"

k() { $TM send-keys -t 0 "$@"; }
typed() { # type text a character at a time, as a person would
    local text="$1" i
    for ((i = 0; i < ${#text}; i++)); do
        $TM send-keys -t 0 -l "${text:i:1}"
        sleep 0.06
    done
}
snap() { [ -n "$SNAP" ] && $TM capture-pane -p -t 0 >"$SNAP/$1.txt"; true; }
wait_for() {
    for _ in $(seq 1 60); do
        $TM capture-pane -p -t 0 2>/dev/null | grep -qF "$1" && return 0
        sleep 0.5
    done
    echo "timed out waiting for: $1" >&2
    $TM capture-pane -p -t 0 >&2
    exit 1
}
[ -n "$SNAP" ] && mkdir -p "$SNAP"

wait_for "ready."
sleep 1.5
snap 1-boot

# A prompt into the focused session, and the reply that buries it. The reply is
# waited for, and then the prompt is checked to be OFF the screen — the case
# this demo exists to show is text search could not see before.
typed "why does the login test flake on CI?"
k Enter
wait_for "● Done."
sleep 1
if $TM capture-pane -p -t 0 | grep -qF "login test flake"; then
    echo "the prompt is still on screen; the filler is too short" >&2
    exit 1
fi
snap 2-buried

# A second session gets a different prompt, so the search has two to rank.
k C-h
sleep 0.6
k Down
sleep 0.6
k Enter
sleep 1
typed "tidy the api error types"
k Enter
wait_for "● Done."
sleep 1.5
snap 3-second

# Search, the words in the other order, and let the answer arrive.
k C-_
sleep 0.8
typed "flake login"
sleep 2
snap 4-results

# Step through the hits: each one previews, scrolling its terminal back.
k Down
sleep 1.5
k Up
sleep 1.5
snap 5-preview

# Open it: the strip closes and the terminal stays on the line.
k Enter
sleep 3
snap 6-landed

k C-q
for _ in $(seq 1 20); do [ -s "$CAST" ] && break; sleep 0.5; done
sleep 1

# Cut the cast where teardown starts, so the GIF loops on the landed frame, not
# a bare shell. Teardown shows the cursor and then leaves the alternate screen;
# the first cursor-show after the last cursor-hide is where it begins. (Not the
# first cursor-show overall: the search strip's input shows the caret while it
# is open.)
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
