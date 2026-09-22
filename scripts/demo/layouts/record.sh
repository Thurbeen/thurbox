#!/usr/bin/env bash
# Record one demo video per layout preset: media/layout-<preset>.{gif,mp4}.
#
#   scripts/demo/layouts/record.sh                  # all four
#   scripts/demo/layouts/record.sh ide focus        # a subset
#
# Each clip is the real TUI, walked through what its preset is for: choosing it
# in the settings panel, the shell (tab or pane) and F8 in and out of it, the
# list with F9 and a session switch, an installed pane's column, and the narrow
# fallback, with the terminal resized under it. The recording is also the
# evidence the preset works, so watch it frame by frame before committing: SNAP
# keeps what the screen held after every step, which is how a key that missed
# is told from one that landed on the wrong pane.
#
# Not VHS: a tape cannot emit F-keys, and every toggle here is one. asciinema
# records the pty, tmux `send-keys` presses the real chords, agg rasterises the
# cast (the pattern of scripts/demo/record-doom.sh).
#
# Hermetic: HOME, every XDG root, THURBOX_CONFIG_DIR/DATA_DIR and the tmux
# socket directory point into a throwaway sandbox, and every inherited THURBOX_*
# and TMUX* variable is dropped — run from inside a thurbox pane, those name the
# operator's own instance. The agent is a made-up script, the sessions and
# repositories are made-up names, and HOME is the sandbox, so every path on
# screen reads `~/work/...`.
#
# Needs: asciinema 2.x, agg, ffmpeg, tmux, git, python3, and thurbox +
# thurbox-cli (THURBOX_BIN, default the checkout's target/debug). agg ships no
# font: FONT_DIR must hold JetBrains Mono and Noto Sans Symbols 2 (the list's
# glyphs), or set FONT_FAMILY to what it does hold.
#
#   WORK=<dir>   where sandboxes and casts go (default: a mktemp dir, removed)
#   SNAP=<dir>   write a text capture of the screen after every step
#   OUT=<dir>    where the media goes (default: media/)
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
BIN=${THURBOX_BIN:-$ROOT/target/debug}
OUT=${OUT:-$ROOT/media}
COLS=${COLS:-120}
ROWS=${ROWS:-34}
FONT_DIR=${FONT_DIR:-$HOME/.local/share/fonts}
FONT_FAMILY=${FONT_FAMILY:-JetBrains Mono,Noto Sans Symbols 2}
SNAP=${SNAP:-}
ALL=(classic split-shell focus ide)
PRESETS=("$@")
[ ${#PRESETS[@]} -gt 0 ] || PRESETS=("${ALL[@]}")

missing=
for tool in asciinema agg ffmpeg tmux git python3; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
for bin in thurbox thurbox-cli; do
    [ -x "$BIN/$bin" ] || missing="$missing $BIN/$bin"
done
[ -z "$missing" ] || { echo "missing:$missing" >&2; exit 2; }

if [ -n "${WORK:-}" ]; then
    mkdir -p "$WORK"
    KEEP=1
else
    WORK=$(mktemp -d)
    KEEP=0
fi
mkdir -p "$OUT"
[ -z "$SNAP" ] || mkdir -p "$SNAP"

# The outer tmux, which holds asciinema, which holds thurbox. Its own socket
# directory, so nothing here can reach a server someone is using.
outer() { env -u TMUX -u TMUX_PANE TMUX_TMPDIR="$S/otmux" tmux -L rec "$@"; }

# One key (or a run of literal text), then a pause long enough to read what it
# did. The pause is what the viewer sees, so it is per step, not global.
k() { outer send-keys -t 0 "$1"; sleep "${2:-0.9}"; }
type_line() { outer send-keys -t 0 -l "$1"; sleep 0.5; outer send-keys -t 0 Enter; sleep "${2:-1.2}"; }
resize() { outer resize-window -t 0 -x "$1" -y "$2"; sleep "${3:-2}"; }
step=0
snap() {
    step=$((step + 1))
    [ -z "$SNAP" ] || outer capture-pane -p -t 0 > "$SNAP/$PRESET-$(printf %02d $step)-$1.txt"
}

sandbox() {
    S="$WORK/$PRESET"
    rm -rf "$S"
    mkdir -p "$S/otmux" "$S/tmux" "$S/cfg" "$S/data" "$S/work"
    cat > "$S/env.sh" <<ENV
for v in \$(env | grep -oE '^(THURBOX|TMUX)[A-Za-z_]*'); do unset "\$v"; done
export HOME="$S" XDG_CONFIG_HOME="$S/.config" XDG_DATA_HOME="$S/.local/share"
export XDG_STATE_HOME="$S/.local/state" XDG_CACHE_HOME="$S/.cache" TMUX_TMPDIR="$S/tmux"
export THURBOX_CONFIG_DIR="$S/cfg" THURBOX_DATA_DIR="$S/data" THURBOX_SOCKET=demo
export PATH="$BIN:\$PATH" TERM=xterm-256color SHELL=/bin/bash LANG=C.UTF-8
export GIT_PAGER=cat PAGER=cat
ENV
    # A prompt that shows where it is, and nothing about who is running it.
    printf "PS1='\\\\w \\\\$ '\n" > "$S/.bashrc"
    # The agent: a line of greeting and an echo of what it is asked. What the
    # clips are about is where it sits, not what it says.
    cat > "$S/demo-agent" <<'AGENT'
#!/usr/bin/env bash
printf '\033[1;36m●\033[0m demo-agent · %s\n\n' "$(basename "$PWD")"
while IFS= read -r -p $'\033[1m❯\033[0m ' line; do
    printf '\033[2m  reading the code…\033[0m\n  ✓ %s\n\n' "$line"
done
AGENT
    chmod +x "$S/demo-agent"
    printf 'default = "demo-agent"\n\n[[agents]]\nname = "demo-agent"\ncommand = "%s"\n' \
        "$S/demo-agent" > "$S/cfg/agents.toml"
    (
        # shellcheck disable=SC1091
        . "$S/env.sh"
        for repo in billing-api web-frontend docs-site; do
            mkdir -p "$S/work/$repo/src"
            printf '# %s\n' "$repo" > "$S/work/$repo/README.md"
            printf 'fn main() {}\n' > "$S/work/$repo/src/main.rs"
            git -C "$S/work/$repo" init -q
            for message in "initial import" "add the config loader" "wire up CI"; do
                printf '%s\n' "$message" >> "$S/work/$repo/CHANGELOG"
                git -C "$S/work/$repo" add -A
                git -C "$S/work/$repo" -c user.email=demo@example.com -c user.name=demo \
                    commit -qm "$message"
            done
        done
        thurbox-cli config accept-interface >/dev/null
        thurbox-cli session create --name fix-invoices --repo-path "$S/work/billing-api" >/dev/null
        thurbox-cli session create --name dark-mode --repo-path "$S/work/web-frontend" >/dev/null
        thurbox-cli session create --name api-docs --repo-path "$S/work/docs-site" >/dev/null
        # An installed pane in a slot of its own — the column split-shell and
        # ide give such panes. Not under classic (unchanged: a column there is
        # placed by hand) nor focus (it keeps them closed).
        if [ "$PRESET" = split-shell ] || [ "$PRESET" = ide ]; then
            thurbox-cli plugin install "$ROOT/examples/panes/tasks" >/dev/null
            thurbox-cli task create --title "Retry failed invoice syncs" >/dev/null
            thurbox-cli task create --title "Dark mode for the settings page" >/dev/null
            thurbox-cli task create --title "Document the refunds endpoint" >/dev/null
        fi
    )
    printf '#!/usr/bin/env bash\n. "%s/env.sh"\ncd "%s/work"\nexec thurbox\n' "$S" "$S" > "$S/run.sh"
    chmod +x "$S/run.sh"
}

teardown() {
    outer kill-server 2>/dev/null || true
    (
        # shellcheck disable=SC1091
        . "$S/env.sh"
        tmux -L demo kill-server 2>/dev/null || true
    )
}

# Settings → layout, one preset along per Right, saved. The row is found by
# its name in the capture rather than by counting rows, so a row added above it
# does not send the recording somewhere else.
choose_in_settings() {
    local steps=$1
    k F6 1.2
    for _ in $(seq 1 30); do
        outer capture-pane -p -t 0 | grep -q '▸ layout ' && break
        k j 0.15
    done
    sleep 0.8
    for _ in $(seq 1 "$steps"); do k Right 0.9; done
    k C-s 1.2
    k Escape 2
}

walk_classic() {
    snap start
    k F6 1.2
    for _ in $(seq 1 30); do
        outer capture-pane -p -t 0 | grep -q '▸ layout ' && break
        k j 0.15
    done
    sleep 1.5
    snap settings
    k Escape 1
    type_line "add a retry to the invoice sync"
    k F8 1.5
    type_line "git log --oneline" 1.5
    snap shell-tab
    k F8 1.5
    k C-h 0.8
    k j 1.5
    snap switched
    k F9 1.5
    snap list-hidden
    k F9 1.2
    resize 70 "$ROWS"
    snap narrow
    resize "$COLS" "$ROWS"
    snap wide
}

walk_split_shell() {
    snap start
    choose_in_settings 1
    snap chosen
    type_line "add a retry to the invoice sync"
    k F8 1.2
    type_line "git log --oneline" 1.5
    snap in-shell
    k F8 1.2
    k C-h 0.8
    k j 1.8
    snap switched
    k F8 1.2
    type_line "ls" 1.2
    resize 70 "$ROWS"
    snap narrow
    type_line "echo still the shell" 1.2
    resize "$COLS" "$ROWS"
    snap wide
    k F5 1.5
    snap tasks
}

walk_focus() {
    snap start
    choose_in_settings 2
    snap chosen
    type_line "add a retry to the invoice sync"
    k F8 1.5
    type_line "git log --oneline" 1.5
    snap shell-tab
    k F8 1.2
    k F9 1.5
    snap list
    k C-h 0.8
    k j 1.8
    snap switched
    k F9 1.5
    resize 70 "$ROWS"
    snap narrow
    resize "$COLS" "$ROWS"
}

walk_ide() {
    snap start
    choose_in_settings 3
    snap chosen
    type_line "add a retry to the invoice sync"
    k F8 1.2
    type_line "git log --oneline" 1.5
    snap in-shell
    k F5 1.2
    k j 0.8
    snap tasks
    k C-h 0.8
    k C-h 0.8
    k C-h 0.8
    k j 1.8
    snap switched
    resize 100 "$ROWS"
    snap middle
    resize 70 "$ROWS"
    snap narrow
    resize "$COLS" "$ROWS"
    snap wide
}

# Cut the cast where thurbox starts exiting (the cursor coming back, or the
# alternate screen leaving): a GIF loops, and a bare shell would be its last
# frame.
trim() {
    python3 - "$1" <<'TRIM'
import json, sys

path = sys.argv[1]
lines = open(path).read().splitlines()
for i, line in enumerate(lines[1:], start=1):
    data = json.loads(line)
    if data[1] == "o" and ("\x1b[?25h" in data[2] or "\x1b[?1049l" in data[2]):
        open(path, "w").write("\n".join(lines[:i]) + "\n")
        break
TRIM
}

for PRESET in "${PRESETS[@]}"; do
    case " ${ALL[*]} " in *" $PRESET "*) ;; *) echo "no preset $PRESET" >&2; exit 2 ;; esac
    echo "==> $PRESET"
    step=0
    sandbox
    CAST="$S/demo.cast"
    outer new-session -d -x "$COLS" -y "$ROWS" \
        "asciinema rec --overwrite --quiet --command '$S/run.sh' '$CAST'"
    # The first frame, not a guess: a cold start boots tmux and three agents.
    for _ in $(seq 1 40); do
        outer capture-pane -p -t 0 2>/dev/null | grep -q 'demo-agent · billing-api' && break
        sleep 1
    done
    sleep 2
    "walk_${PRESET//-/_}"
    sleep 2.5
    k C-q 3
    for _ in $(seq 1 20); do [ -s "$CAST" ] && break; sleep 1; done
    teardown
    trim "$CAST"
    agg --font-dir "$FONT_DIR" --font-family "$FONT_FAMILY" --font-size 14 \
        --idle-time-limit 2 --fps-cap 15 --theme asciinema "$CAST" "$OUT/layout-$PRESET.gif"
    [ -s "$OUT/layout-$PRESET.gif" ] || { echo "agg produced no GIF" >&2; exit 1; }
    # `-r` because ffmpeg reads a GIF's frame delays as ~100 fps and would pad
    # the mp4 with duplicate frames.
    ffmpeg -y -loglevel error -i "$OUT/layout-$PRESET.gif" -r 15 \
        -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2" \
        -c:v libx264 -preset slow -crf 30 -pix_fmt yuv420p -movflags +faststart \
        "$OUT/layout-$PRESET.mp4"
    ls -la "$OUT/layout-$PRESET.gif" "$OUT/layout-$PRESET.mp4"
done

[ "$KEEP" = 1 ] || rm -rf "$WORK"
