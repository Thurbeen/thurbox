#!/usr/bin/env bash
# Record the settings Interface tab on a fixed, filler setup, with VHS.
#
#   scripts/demo/record-interface-tab.sh [label]
#
# Writes media/interface-tab-<label>.gif (label defaults to `after`). BIN_DIR
# picks the binaries (default: this checkout's target/debug), so the same setup
# can be recorded against an older build for a before/after pair:
#
#   BIN_DIR=/path/to/old/target/debug scripts/demo/record-interface-tab.sh before
#
# The setup is what makes the tab worth looking at: the shipped panes, two
# example packages installed from a local directory (`top` asks to run programs
# and is left untrusted; `tasks` is turned off), a shipped pane edited by hand
# and a pane of your own in a slot the layout does not place. Everything lives
# in a throwaway HOME/config/data/tmux under $TMPDIR and is removed afterwards.
#
# The thurbox theme is the shipped `default`, written to metadata.active_theme
# explicitly rather than left to whatever a fresh profile falls back to.
#
# Needs: vhs (+ ttyd, ffmpeg), sqlite3, python3, tmux.
set -euo pipefail

LABEL=${1:-after}
REPO=$(cd -- "$(dirname -- "$0")/../.." && pwd)
BIN_DIR=${BIN_DIR:-$REPO/target/debug}
ROOT=${TMPDIR:-/tmp}/tbx-interface-demo
OUT=$REPO/media/interface-tab-$LABEL.gif

rm -rf "$ROOT"
mkdir -p "$ROOT"/home "$ROOT"/config "$ROOT"/data "$ROOT"/tmux "$ROOT"/pkgs
trap 'tmux -S "$ROOT/tmux/tmux-$(id -u)/tbx-demo" kill-server 2>/dev/null || true; rm -rf "$ROOT"' EXIT

export HOME=$ROOT/home
export XDG_CONFIG_HOME=$ROOT/home/.config XDG_DATA_HOME=$ROOT/home/.local/share
export THURBOX_CONFIG_DIR=$ROOT/config THURBOX_DATA_DIR=$ROOT/data
export TMUX_TMPDIR=$ROOT/tmux THURBOX_SOCKET=tbx-demo
printf '[features]\nautomations = false\nversion_check = false\nauto_update = false\n' \
    >"$ROOT/config/settings.toml"

CLI=$BIN_DIR/thurbox-cli
UI=$ROOT/config/ui
cp -r "$REPO/examples/panes/top" "$REPO/examples/panes/tasks" "$ROOT/pkgs/"
"$CLI" plugin list --text >/dev/null
"$CLI" plugin install "$ROOT/pkgs/top" --text >/dev/null
"$CLI" plugin install "$ROOT/pkgs/tasks" --text >/dev/null
printf '\n-- a local tweak\n' >>"$UI/plugins/10_sessions.lua"
cat >"$UI/plugins/90_notes.lua" <<'LUA'
return {
  name = "notes",
  slot = "notes",
  render = function() return { type = "text", text = "notes" } end,
}
LUA
# VHS can send neither F6 nor Ctrl+, so settings is rebound for the recording,
# as scripts/demo/record.sh does.
python3 - "$ROOT/config/ui.json" "$UI/plugins/80_tasks.lua" <<'PY'
import json, sys
json.dump(
    {"bindings": {"settings.open": "ctrl+b"}, "disabled": [sys.argv[2]],
     "settings": {}, "trusted": {}},
    open(sys.argv[1], "w"),
    indent=2,
)
PY
"$CLI" session list --text >/dev/null 2>&1 || true
sqlite3 "$ROOT/data/thurbox.db" "INSERT INTO metadata (key, value) VALUES ('active_theme', 'default')
  ON CONFLICT(key) DO UPDATE SET value = excluded.value;"

# Walk the panes down to the edited one, press `r` once (which, on this branch,
# only asks), step away, then scroll to the end of the list.
cat >"$ROOT/demo.tape" <<TAPE
Output "$OUT"
Set Shell "bash"
Set FontSize 16
Set Width 1500
Set Height 860
Set Padding 12
Hide
Type "env HOME=$HOME XDG_CONFIG_HOME=$XDG_CONFIG_HOME XDG_DATA_HOME=$XDG_DATA_HOME THURBOX_CONFIG_DIR=$THURBOX_CONFIG_DIR THURBOX_DATA_DIR=$THURBOX_DATA_DIR TMUX_TMPDIR=$TMUX_TMPDIR THURBOX_SOCKET=$THURBOX_SOCKET PATH=$BIN_DIR:/usr/bin:/bin $BIN_DIR/thurbox"
Enter
Sleep 4s
Show
Sleep 1s
Ctrl+B
Sleep 1200ms
Type "]"
Sleep 3s
Type "j"
Sleep 2500ms
Type "j"
Sleep 2500ms
Type "j"
Sleep 2500ms
Type "j"
Sleep 2500ms
Type "r"
Sleep 3500ms
Type "j"
Sleep 1500ms
Down@150ms 14
Sleep 2500ms
Escape
Sleep 800ms
Ctrl+Q
Sleep 1s
TAPE
vhs "$ROOT/demo.tape"
echo "wrote $OUT"
