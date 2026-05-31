# VHS tape — Thurbox file manager (file viewer) feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh file-manager`, which
# builds the dev binary, sets up the isolated env, seeds one session per real
# agent CLI in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+E toggles the file viewer; Ctrl+L/H cycle pane focus. (VHS has no F-keys.)

Output docs/media/thurbox-file-manager.gif
Output docs/media/thurbox-file-manager.mp4

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

# Open the file viewer (Ctrl+E = alias for F3): the sample repo's file tree.
Ctrl+E
Sleep 2s

# Focus the file viewer pane.
Ctrl+L
Sleep 1200ms

# Browse the tree: move down, expand a directory, open a file.
Down
Sleep 700ms
Down
Sleep 700ms
Enter
Sleep 1500ms
Down
Sleep 700ms
Down
Sleep 700ms
Enter
Sleep 1800ms

# Incremental search beat.
Type "/"
Sleep 600ms
Type "lib"
Sleep 1500ms
Escape
Sleep 1s

# Return focus to the session list, hide the viewer.
Ctrl+H
Sleep 800ms
Ctrl+E
Sleep 1200ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
