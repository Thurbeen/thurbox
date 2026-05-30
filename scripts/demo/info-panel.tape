# VHS tape — Thurbox info panel feature clip.
#
# Do NOT run directly — run `scripts/demo/record-agents.sh info-panel`, which
# builds the dev binary, sets up the isolated env, seeds one session per real
# agent CLI in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+B toggles the info panel; Ctrl+J/K move between sessions. (No F-keys.)

Output docs/media/thurbox-info-panel.gif
Output docs/media/thurbox-info-panel.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

# Launch the dev binary (isolated env from record-agents.sh).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the real agent CLIs finish booting.
Sleep 2500ms

# Open the info panel (Ctrl+B = alias for F2): name/status/agent + CPU/RAM gauges.
Ctrl+B
Sleep 3s

# Move between sessions — the info panel tracks the selected session.
Ctrl+J
Sleep 2s
Ctrl+J
Sleep 2s
Ctrl+K
Sleep 2s

# Close the info panel.
Ctrl+B
Sleep 1200ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
