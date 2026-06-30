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
#   Ctrl+G = keybindings help, Ctrl+J = next session, Ctrl+X = settings.
# Ctrl+A (global search) and Ctrl+X (settings) are remapped by capture.sh via a
# seeded keybindings.json, because VHS cannot type their real chords (Ctrl+/, Ctrl+,).

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

# 11-14 — the native code-review view. The "review" session is seeded last, so
# it's the one selected on launch. ToggleReview is a terminal-passthrough chord,
# so a bare Ctrl+R is forwarded to the agent while the terminal is focused — focus
# the session list first (Ctrl+H, never passthrough) so Ctrl+R runs as a global
# action and opens the diff for the selected "review" session.
Ctrl+H
Sleep 500ms
Ctrl+R
Sleep 1800ms
# Move onto a changed line so the comment anchors to it.
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms
Type "j"
Sleep 900ms
Screenshot "__SHOT_DIR__/11-code-review.png"
# Inline comment compose + classification badge (Tab cycles the type).
Type "c"
Sleep 1000ms
Type "Use checked_add to surface overflow"
Sleep 600ms
Tab
Sleep 800ms
Screenshot "__SHOT_DIR__/12-code-review-comment.png"
Ctrl+S
Sleep 1200ms
# Side-by-side layout.
Type "v"
Sleep 1200ms
Screenshot "__SHOT_DIR__/13-code-review-sidebyside.png"
Type "v"
Sleep 600ms
# Target picker (branch / working / per-commit).
Type "t"
Sleep 1200ms
Screenshot "__SHOT_DIR__/14-code-review-targets.png"
Escape
Sleep 500ms
Escape
Sleep 800ms

# 02 — select a different session (terminal still focused)
Ctrl+J
Sleep 1500ms
Screenshot "__SHOT_DIR__/02-second-session.png"

# Focus the session list. The panel-toggle chords below (Ctrl+B/E/W/P) are
# terminal-PASSTHROUGH actions: while a session terminal is focused they are
# forwarded to the agent CLI instead of running the thurbox command, so they
# must be issued from the session list. Ctrl+H (focus previous pane) is NOT a
# passthrough, so it reliably moves focus Terminal -> session list. Each toggle
# below returns focus to the session list when closed, so one Ctrl+H suffices.
Ctrl+H
Sleep 700ms

# 03 — info panel (toggle leaves focus on the session list)
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

# 11 — settings panel. Default chord is Ctrl+, / F6, neither typeable by VHS;
# capture.sh remaps OpenSettings -> Ctrl+X for the tape.
Ctrl+X
Sleep 1500ms
Screenshot "__SHOT_DIR__/11-settings-panel.png"
Escape
Sleep 700ms

# 12 — automation editor (multi-line wrapping prompt + modal button footer).
# Ctrl+P opens the automations list; Enter edits the seeded "nightly-triage".
Ctrl+P
Sleep 900ms
Enter
Sleep 1500ms
Screenshot "__SHOT_DIR__/12-automation-editor.png"
Escape
Sleep 700ms

# 13 — task editor (in-pane, central-pane). Ctrl+W focuses the tasks panel;
# Enter opens the editor for the selected task.
Ctrl+W
Sleep 900ms
Enter
Sleep 1500ms
Screenshot "__SHOT_DIR__/13-task-editor.png"
Escape
Sleep 700ms

# Quit cleanly.
Ctrl+Q
Sleep 1s
