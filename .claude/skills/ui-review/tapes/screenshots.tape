# VHS tape for the ui-review skill — captures a PNG of each thurbox TUI state.
#
# Do NOT run this directly. scripts/capture.sh substitutes the __PLACEHOLDERS__
# (__SHOT_DIR__, __WIDTH__, __HEIGHT__) and exports $THURBOX_BIN, then runs VHS in
# an isolated sandbox. VHS Screenshot paths are literal (no env expansion), which is
# why capture.sh sed-substitutes the absolute screenshot dir here.
#
# VHS has no function-key commands, so this uses thurbox's Ctrl aliases:
#   Ctrl+B = info panel, Ctrl+E = file viewer, Ctrl+W = tasks, Ctrl+P = automations,
#   Ctrl+A = global search, Ctrl+Y = theme picker, Ctrl+N = new session,
#   Ctrl+G = keybindings help, Ctrl+J = next session.

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

# Let panes boot.
Sleep 3s

# 01 — default view: session list + terminal
Screenshot "__SHOT_DIR__/01-session-list.png"

# 02 — select a different session
Ctrl+J
Sleep 1500ms
Screenshot "__SHOT_DIR__/02-second-session.png"

# 03 — info panel
Ctrl+B
Sleep 1500ms
Screenshot "__SHOT_DIR__/03-info-panel.png"
Ctrl+B
Sleep 700ms

# 04 — file viewer
Ctrl+E
Sleep 1500ms
Screenshot "__SHOT_DIR__/04-file-viewer.png"
Ctrl+E
Sleep 700ms

# 05 — tasks panel
Ctrl+W
Sleep 1500ms
Screenshot "__SHOT_DIR__/05-tasks-panel.png"
Ctrl+W
Sleep 700ms

# 06 — automations pane
Ctrl+P
Sleep 1500ms
Screenshot "__SHOT_DIR__/06-automations.png"
Escape
Sleep 700ms

# 07 — global search strip (with a query so it shows results)
Ctrl+A
Sleep 800ms
Type "test"
Sleep 1200ms
Screenshot "__SHOT_DIR__/07-global-search.png"
Escape
Sleep 700ms

# 08 — theme picker
Ctrl+Y
Sleep 1500ms
Screenshot "__SHOT_DIR__/08-theme-picker.png"
Escape
Sleep 700ms

# 09 — new-session / repo picker
Ctrl+N
Sleep 1500ms
Screenshot "__SHOT_DIR__/09-repo-picker.png"
Escape
Sleep 700ms

# 10 — keybindings help + editor
Ctrl+G
Sleep 1500ms
Screenshot "__SHOT_DIR__/10-keybindings.png"
Escape
Sleep 500ms

# Quit cleanly.
Ctrl+Q
Sleep 1s
