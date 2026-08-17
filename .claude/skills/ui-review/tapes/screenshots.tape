# VHS tape for the ui-review skill — captures a PNG of each thurbox TUI state.
#
# Do NOT run this directly. scripts/capture.sh substitutes the __PLACEHOLDERS__
# (__SHOT_DIR__, __WIDTH__, __HEIGHT__) and exports $THURBOX_BIN, then runs VHS in
# an isolated sandbox. VHS Screenshot paths are literal (no env expansion), which is
# why capture.sh sed-substitutes the absolute screenshot dir here.
#
# Chords used, all Ctrl+<letter> because VHS emits no F-keys and no
# Ctrl+punctuation:
#   Ctrl+J next session   Ctrl+Y theme picker   Ctrl+N new session
#   Ctrl+G help           Ctrl+T shell          Ctrl+H focus previous pane
#   Ctrl+A search   (really Ctrl+/ — rebound by capture.sh via ui.json)
#   Ctrl+B settings (really Ctrl+, / F6 — likewise)
#
# Focus matters. A chord a plugin flags as passthrough is forwarded to the agent
# CLI while a session terminal is focused, so the tape presses Ctrl+H first —
# focus-cycling is never deferred, which is what makes it the reliable way out of
# a focused terminal.

# VHS still requires at least one Output; we use a throwaway one and rely on
# Screenshot for the real artifacts.
Output "__SHOT_DIR__/_vhs-scratch.mp4"

Set Shell "bash"
Set FontSize 18
Set Width __WIDTH__
Set Height __HEIGHT__
Set Padding 16
Set Theme "Catppuccin Mocha"

# --- Launch the dev TUI (isolated env from capture.sh) ----------------------
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the real agent CLIs finish booting so the panes hold live content rather
# than an empty terminal.
Sleep 3500ms

# 01 — default view: session list + agent terminal
Screenshot "__SHOT_DIR__/01-session-list.png"

# 02 — a different session selected. Ctrl+J is navigation, never passthrough, so
# it works with the terminal still focused.
Ctrl+J
Sleep 1500ms
Screenshot "__SHOT_DIR__/02-second-session.png"

# Out of the terminal for the rest of the tape.
Ctrl+H
Sleep 700ms

# 03 — the search strip. The point of this screen is that matches highlight in
# the session list itself and non-matching rows dim, rather than being reprinted
# in the strip — so the query has to match some sessions and not others.
Ctrl+A
Sleep 800ms
Type "re"
Sleep 1400ms
Screenshot "__SHOT_DIR__/03-search.png"
Escape
Sleep 700ms

# 04 — theme picker: 36 palettes under Dark/Light headers drawn inside their
# entry's row.
Ctrl+Y
Sleep 1500ms
Screenshot "__SHOT_DIR__/04-theme-picker.png"

# 05 — the same picker filtered. Letters only append after `/`, and the count is
# live, so this shows both the query and matched/total.
Type "/"
Sleep 500ms
Type "light"
Sleep 1200ms
Screenshot "__SHOT_DIR__/05-theme-filter.png"
Escape
Sleep 400ms
Escape
Sleep 700ms

# 06 — new session, first step: the repo picker.
Ctrl+N
Sleep 1600ms
Screenshot "__SHOT_DIR__/06-new-session-repo.png"

# 07 — a later step of the same flow. The seeded repo is preselected, so Enter
# advances; this captures whichever step follows (name / agent) with the wizard's
# own hint line.
Enter
Sleep 1400ms
Screenshot "__SHOT_DIR__/07-new-session-agent.png"
Escape
Sleep 800ms

# 08 — keybindings help. Rendered FROM the registry, so it lists exactly what the
# loaded plugins declared: there is no hardcoded table to drift.
Ctrl+G
Sleep 1600ms
Screenshot "__SHOT_DIR__/08-help.png"
Escape
Sleep 700ms

# 09 — settings: plugin settings (effective next frame) above core settings
# (settings.toml), with restart-only rows marked.
Ctrl+B
Sleep 1600ms
Screenshot "__SHOT_DIR__/09-settings.png"

# 10 — the Interface tab: every file, where it came from, whether it is on screen.
# This is the screen that has no v1 counterpart.
Type "]"
Sleep 1600ms
Screenshot "__SHOT_DIR__/10-interface-tab.png"

# 11 — turn a pane off and watch the arrangement close up around it. Row 0 is
# `65_search.lua`, whose slot is empty while search is closed, so toggling that
# would show no reflow at all — one `j` first, onto `plugins/10_sessions.lua`.
# Movement here is j/k as the modal's own hint says; arrows are not bound for up.
Type "j"
Sleep 700ms
Space
Sleep 1400ms
Escape
Sleep 1600ms
Screenshot "__SHOT_DIR__/11-plugin-off.png"

# Put it back, so the shell screenshot below is of the normal arrangement.
Ctrl+B
Sleep 1000ms
Type "]"
Sleep 1000ms
Type "j"
Sleep 600ms
Space
Sleep 1200ms
Escape
Sleep 1200ms

# 12 — the per-session shell pane.
Ctrl+T
Sleep 2s
Screenshot "__SHOT_DIR__/12-shell-pane.png"

# Quit cleanly (detaches sessions).
Ctrl+Q
Sleep 1s
