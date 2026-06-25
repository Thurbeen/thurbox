# VHS tape — Thurbox info panel feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh info-panel`, which
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

# Launch the dev binary (isolated env from record.sh).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the real agent CLIs finish booting.
Sleep 2500ms

# On launch the active session's TERMINAL is focused (session restore focuses
# it), and Ctrl+B (ToggleInfoPanel) is a terminal-passthrough chord — so from
# the terminal it would be sent to the agent's PTY instead of toggling the
# panel. Ctrl+H (focus previous pane) is a navigation chord that is never passed
# through, so it reliably moves focus to the session list first.
Ctrl+H
Sleep 600ms

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
