#!/usr/bin/env bash
#
# Black-box smoke test for the thurbox TUI: launch the *real* `thurbox` binary
# inside a throwaway tmux pane, drive it with keystrokes, and assert on the
# frames it actually paints — the one thing the in-process tests can't cover,
# since they never touch a terminal. (It used to say "unlike
# src/app/acceptance.rs"; that harness went with v1's `src/app`.)
#
# Everything is isolated from your real environment, mirroring scripts/demo/
# record.sh: a temp HOME + XDG dirs, and a private TMUX_TMPDIR so both the outer
# "driver" tmux (socket `tui-smoke`) and thurbox's own dev socket
# (`thurbox-dev`) live in — and are torn down from — the temp dir. It never
# touches your real ~/.config/thurbox or any tmux server you have running.
#
# Usage:
#   scripts/dev/smoke/tui-smoke.sh          # build (debug) + run the smoke test
#   THURBOX_BIN=/path/to/thurbox \
#     scripts/dev/smoke/tui-smoke.sh        # test a prebuilt binary (skip build)
#
# Requires: tmux >= 3.2, cargo (unless THURBOX_BIN is set).
# Exit status: 0 = all assertions passed, non-zero = a failure (printed).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SOCKET="tui-smoke"
SESSION="thurbox-smoke"
COLS=120
ROWS=40

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok() { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v tmux >/dev/null || die "tmux not found (need >= 3.2)"

# --- build (unless a binary was provided) — before the HOME override below ----
if [ -n "${THURBOX_BIN:-}" ]; then
  BIN="$THURBOX_BIN"
  [ -x "$BIN" ] || die "THURBOX_BIN=$BIN is not executable"
else
  log "building thurbox (debug)"
  ( cd "$REPO_ROOT" && cargo build --bin thurbox >&2 )
  BIN="$REPO_ROOT/target/debug/thurbox"
fi
[ -x "$BIN" ] || die "thurbox binary not found at $BIN"

# --- isolated environment (shared dev-sandbox helper) ------------------------
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh   # hermetic: temp HOME/XDG so it never touches real config

# shellcheck disable=SC2317,SC2329 # body runs via the `trap` below; not unreachable
cleanup() {
  # The outer "driver" tmux (socket `tui-smoke`) lives in the same private
  # TMUX_TMPDIR; kill it, then let the helper kill thurbox-dev + wipe the root.
  tmux -L "$SOCKET" kill-server >/dev/null 2>&1 || true
  tbx_sandbox_teardown
}
trap cleanup EXIT INT TERM

# --- launch the TUI in an isolated tmux pane ---------------------------------
log "launching TUI in tmux ($COLS x $ROWS)"
tmux -L "$SOCKET" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$BIN"

# Capture the current pane as plain text.
capture() { tmux -L "$SOCKET" capture-pane -p -t "$SESSION"; }

# Poll until `capture` contains $1 (or time out). tmux send-keys is fire-and-
# forget, so we wait on the rendered result rather than sleeping a fixed time.
wait_for() {
  local needle="$1" tries="${2:-50}"
  for _ in $(seq 1 "$tries"); do
    if capture | grep -qF "$needle"; then return 0; fi
    sleep 0.1
  done
  printf '\n--- final frame ---\n%s\n-------------------\n' "$(capture)" >&2
  die "timed out waiting for: $needle"
}

# The inverse: poll until $1 is GONE. Needed after an Escape, and not merely for
# tidiness — `send` is fire-and-forget, so an Escape immediately followed by
# another chord reaches the terminal as one burst, and `ESC` + a byte is exactly
# what an ESC-prefixed (alt+key) sequence looks like. Sending the next key without
# waiting for the overlay to actually close swallowed it, and the failure then
# surfaced one assertion later as "the picker never opened".
wait_gone() {
  local needle="$1" tries="${2:-50}"
  for _ in $(seq 1 "$tries"); do
    if ! capture | grep -qF "$needle"; then return 0; fi
    sleep 0.1
  done
  printf '\n--- final frame ---\n%s\n-------------------\n' "$(capture)" >&2
  die "timed out waiting for this to disappear: $needle"
}

send() { tmux -L "$SOCKET" send-keys -t "$SESSION" "$@"; }

# --- assertions --------------------------------------------------------------
# 1. It boots and paints its chrome.
wait_for "thurbox"
ok "TUI booted and rendered its header"
wait_for "No sessions yet"
ok "empty-state hint is shown"

# 2. F1 opens the keybindings help overlay.
#
# Asserted on the overlay's own chrome — its title and its footer — because those
# are pinned wherever the list is scrolled. The needle used to be `Quit`, which
# broke: the reserved chords render after every plugin group, so as panes declared
# more keys `Quit` slid below the fold and a passing overlay read as a timeout.
# Any needle that depends on scroll position will do that again.
send F1
wait_for "Keybindings"
wait_for "rebind"
ok "F1 opened the help overlay"
# And it rendered the registry, not just a frame: one real binding row.
wait_for "next session"
ok "the help overlay lists declared bindings"
send Escape
wait_gone "Keybindings"
ok "Escape closed it"

# 3. Ctrl+Y opens the theme picker, listing built-in palettes.
#
# `/ filter themes` rather than the picker's title: the footer band already says
# `Theme · F4`, so the title alone would match with no picker open at all.
send C-y
wait_for "/ filter themes"
ok "Ctrl+Y opened the theme picker"
# Grouped and populated. `Dark` is the group header, which does not move when the
# presets are reordered — unlike any single palette name in a 36-entry list.
wait_for "Dark"
ok "the picker groups and lists the built-in palettes"
send Escape
wait_gone "filter themes"
ok "Escape closed it"

# 4. Ctrl+Q exits cleanly (the tmux session ends when thurbox returns).
send C-q
for _ in $(seq 1 50); do
  if ! tmux -L "$SOCKET" has-session -t "$SESSION" 2>/dev/null; then
    ok "Ctrl+Q quit and the process exited"
    log "all smoke assertions passed"
    exit 0
  fi
  sleep 0.1
done
die "TUI did not exit after Ctrl+Q"
