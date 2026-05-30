# VHS tape for the Thurbox demo video.
#
# Do NOT run this directly — run `scripts/demo/record.sh`, which builds the dev
# binary, sets up an isolated environment, pre-seeds demo sessions, and exports
# the env vars this tape relies on ($THURBOX_BIN). See that script for context.
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. VHS has no function-key
# commands, so this uses the Ctrl-key bindings only (Ctrl+Y for theme, etc.).

Output docs/media/thurbox-demo.gif
Output docs/media/thurbox-demo.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

# Launch the dev binary (isolated thurbox-dev socket + XDG dirs from record.sh).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the pre-seeded sessions stream their transcripts.
Sleep 2500ms

# Cycle through the sessions in the list (Ctrl+J = next, Ctrl+K = previous).
Ctrl+J
Sleep 2s
Ctrl+J
Sleep 2s
Ctrl+K
Sleep 1500ms

# Move focus across panes (Ctrl+L = next pane, Ctrl+H = previous).
Ctrl+L
Sleep 1500ms
Ctrl+L
Sleep 1500ms

# Switch the TUI theme (Ctrl+Y opens the theme picker).
Ctrl+Y
Sleep 1s
Down
Sleep 600ms
Enter
Sleep 1500ms

# Create a new session to show the repo picker (Ctrl+N), then cancel out.
Ctrl+N
Sleep 2500ms
Escape
Sleep 1s

# Final beat, then quit (detaches sessions).
Sleep 1500ms
Ctrl+Q
Sleep 1s
