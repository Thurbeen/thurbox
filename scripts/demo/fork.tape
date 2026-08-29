# VHS tape — Thurbox session forking (parent sessions) feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh fork`, which builds the
# dev binary, sets up the isolated env, seeds one session per real agent CLI
# in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+F forks the active session: a name modal opens pre-filled with
# `<name>-fork`, and the spawned copy records the source as its parent — the
# session list nests children under their parent with a `└` tree prefix, and
# the info panel (Ctrl+B) shows a `Parent:` row.

Output media/thurbox-fork.gif
Output media/thurbox-fork.mp4

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

# Let the real agent CLIs finish booting. The TUI adopts the seeded sessions
# with the terminal focused, so hop back to the session list first (Ctrl+F
# would otherwise forward to the agent's PTY).
Sleep 2500ms
Ctrl+H
Sleep 800ms

# Fork the active session (Ctrl+F): a name modal opens, pre-filled with
# `<name>-fork`. Accept it as-is.
Ctrl+F
Sleep 2s
Enter
Sleep 4s

# The fork spawned nested under its parent (`└` tree prefix) and now has its
# terminal focused. Back to the list, then hop up to the parent ...
Ctrl+H
Sleep 800ms
Ctrl+K
Sleep 1500ms

# ... and fork it again, this time tweaking the suggested name.
Ctrl+F
Sleep 1500ms
Type "2"
Sleep 800ms
Enter
Sleep 4s

# Two children now nest under the same parent. Walk the tree — Ctrl+J/K follow
# the parent → children order automatically.
Ctrl+H
Sleep 800ms
Ctrl+K
Sleep 1200ms
Ctrl+K
Sleep 1200ms
Ctrl+J
Sleep 1200ms

# The info panel shows the lineage: a `Parent:` row on the selected child.
Ctrl+B
Sleep 3500ms
Ctrl+J
Sleep 2500ms
Ctrl+B
Sleep 1200ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
