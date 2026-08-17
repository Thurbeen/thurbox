# VHS tape for the Thurbox REAL-AGENTS demo video.
#
# Do NOT run this directly — run `scripts/demo/record.sh`, which builds
# the dev binary, sets up an isolated environment, seeds one session per real
# agent CLI, and exports $THURBOX_BIN. See that script for context.
#
# VHS has no function-key commands, so every beat here uses a chord VHS can
# actually send. The info panel, file viewer and code-review view this tape used
# to open no longer exist — the interface is the plugin set under `ui/`, and the
# clip that shows *that* is interface.tape.

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

# Focus a session's terminal and back out again: Ctrl+L walks the focus ring
# (session list -> agent), Ctrl+H walks it back. The ring is built from whichever
# panes are loaded, so it is the plugin set that decides what focus can reach.
Ctrl+L
Sleep 2200ms
Ctrl+H
Sleep 1200ms

# Global search (rebound to Ctrl+A for the recording — really Ctrl+/): it matches
# session names, agents, branches and repos, AND the text on each agent's screen.
Ctrl+A
Sleep 1200ms
Type "osc"
Sleep 2000ms
Escape
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
