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

# On launch the active session's TERMINAL is focused (session restore focuses
# it), and Ctrl+E (ToggleFileViewer) is a terminal-passthrough chord — so from
# the terminal it would be sent to the agent's PTY instead of toggling the
# panel. Ctrl+H (focus previous pane) is a navigation chord that is never passed
# through, so it reliably moves focus to the session list first.
Ctrl+H
Sleep 600ms

# Open the file viewer (Ctrl+E = alias for F3): the sample repo's file tree.
Ctrl+E
Sleep 2s

# Focus the file viewer pane. From the session list, Ctrl+H wraps backward
# around the focus ring straight to the file viewer (its last stop).
Ctrl+H
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

# Return focus to the session list (Ctrl+L wraps forward from the file viewer),
# then hide the viewer with Ctrl+E now that the terminal isn't focused.
Ctrl+L
Sleep 800ms
Ctrl+E
Sleep 1200ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
