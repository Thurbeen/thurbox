# VHS tape for the Thurbox REAL-AGENTS demo video.
#
# Do NOT run this directly — run `scripts/demo/record.sh`, which builds
# the dev binary, sets up an isolated environment, seeds one session per real
# agent CLI, and exports $THURBOX_BIN. See that script for context.
#
# VHS has no function-key commands, so this uses the Ctrl aliases added for the
# panel toggles: Ctrl+B = info panel, Ctrl+E = file viewer.

Output docs/media/thurbox-demo.gif
Output docs/media/thurbox-demo.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

# Launch the dev binary (isolated env from record.sh).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the real agent CLIs finish booting in their panes.
Sleep 3s

# Walk through each pre-seeded agent session (claude / opencode / codex / antigravity).
Ctrl+J
Sleep 2500ms
Ctrl+J
Sleep 2500ms
Ctrl+J
Sleep 2500ms
Ctrl+J
Sleep 2500ms

# Show the info panel (Ctrl+B = alias for F2): per-session details + metrics.
Ctrl+B
Sleep 3s

# Show the file viewer (Ctrl+E = alias for F3): the sample repo's file tree.
Ctrl+E
Sleep 2s

# Focus the file viewer pane and browse the tree.
Ctrl+L
Sleep 1s
Down
Sleep 600ms
Down
Sleep 600ms
Enter
Sleep 1200ms
Down
Sleep 800ms

# Return focus to the session list, hide the panels again.
Ctrl+H
Sleep 800ms
Ctrl+E
Sleep 600ms
Ctrl+B
Sleep 1200ms

# --- Session-creation workflow -----------------------------------------------
# Ctrl+N opens the repo picker. The sample repo is already bookmarked (the
# seeded sessions registered it), so we select it and walk the full flow:
# repo picker -> session name -> agent picker -> spawn.
Ctrl+N
Sleep 2s

# Select the highlighted repo (Space toggles selection), then confirm (Enter).
Space
Sleep 1200ms
Enter
Sleep 1500ms

# Session-name prompt: type a name, then confirm.
Type "review-bot"
Sleep 1500ms
Enter
Sleep 1500ms

# Agent picker (shown because several agents are registered): choose one.
Sleep 1s
Down
Sleep 700ms
Down
Sleep 900ms
Enter
Sleep 1500ms

# The new session appears in the list and its agent boots.
Sleep 3s

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
