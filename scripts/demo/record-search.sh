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
# loop that echoes each prompt and then prints a page of work — because thurbox
# is agent-neutral and the point is the terminal's scrollback, not any one CLI.
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
FONT_DIR="${FONT_DIR:-/usr/share/fonts}"
FONT_FAMILY="${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono}"

missing=
for tool in asciinema agg tmux git; do
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
# A stand-in coding agent: echo the prompt, then a page of "work" — enough to
# push the prompt well out of view.
printf 'demo agent ready. type a prompt.\n'
while printf '\n› ' && IFS= read -r prompt; do
    printf 'prompt received: %s\n' "$prompt"
    i=1
    while [ "$i" -le 120 ]; do
        printf '  step %3d  reading src/module_%02d.rs … ok\n' "$i" $((i % 37))
        i=$((i + 1))
    done
    printf 'done.\n'
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

wait_for "demo agent ready"
sleep 1.5
snap 1-boot

# A prompt into the focused session, and the work that buries it.
typed "why does the login test flake on CI?"
k Enter
wait_for "done."
sleep 1
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
wait_for "done."
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
    --idle-time-limit 2 --theme asciinema "$CAST" "$OUT"
ls -lh "$OUT"
