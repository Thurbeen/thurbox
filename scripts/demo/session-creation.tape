# VHS tape — Thurbox session-creation workflow feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh session-creation`,
# which builds the dev binary, sets up the isolated env, seeds one session per
# real agent CLI in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+N opens the repo picker. Flow: repo picker -> name -> agent picker -> spawn.

Output docs/media/thurbox-session-creation.gif
Output docs/media/thurbox-session-creation.mp4

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

# Let the real agent CLIs finish booting.
Sleep 2500ms

# Open the repo picker (Ctrl+N). The sample repo is already bookmarked (the
# seeded sessions registered it).
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
Sleep 800ms
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
