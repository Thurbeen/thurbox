# VHS tape for the main Thurbox demo video.
#
# Do NOT run this directly — run `scripts/demo/record.sh`, which builds the dev
# binary, sets up an isolated environment, seeds one session per real agent CLI,
# installs the `docs/examples/` demo (layout + tasks + top) and trusts the one
# that runs a program, then exports $THURBOX_BIN. See that script for context.
#
# What this clip is for: the interface as somebody's own arrangement. The two
# panes stacked on the right are NOT bundled — they are `ui-plugins/tasks/` and
# `ui-plugins/top/`, placed by `docs/examples/layout.lua`, and they are
# on screen from the first frame because that is the point. A viewer who takes
# nothing else away should notice that two of the four panes came from a
# directory they could edit.
#
# Session creation is deliberately NOT here any more — it has its own clip
# (session-creation.tape), and the space it took is better spent on the panes.
#
# Chords: VHS sends no F-keys and no Ctrl+punctuation, so record.sh rebinds four
# actions in `ui.json` for the recording only:
#   Ctrl+A search (really Ctrl+/)      Ctrl+E tasks  (really F5)
#   Ctrl+B settings (really Ctrl+,/F6) Ctrl+W top    (really F7)

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

# Let the real agent CLIs finish booting in their panes, and give the `top` pane
# a beat to get its first answer back — it asks on the first frame and a worker
# replies, so the gauges arrive a moment after everything else.
Sleep 3500ms

# Walk the seeded agent sessions. The `top` pane follows the selection: `run`
# executes in the selected session's directory, on that session's host, so the
# gauges re-read as the cursor moves — which is the whole reason it is a pane and
# not a status line.
Ctrl+J
Sleep 2200ms
Ctrl+J
Sleep 2200ms
Ctrl+J
Sleep 2200ms

# --- The tasks pane ----------------------------------------------------------
# Not shipped. It reads `thurbox.tasks` — the same records `thurbox-cli task`
# writes — and sends its changes back as commands, so it needs no capability at
# all.
Ctrl+E
Sleep 1600ms

# `space` cycles a task todo → in progress → done. The glyph and the colour both
# change, and the count underneath moves with them.
Space
Sleep 1600ms
Type "j"
Sleep 700ms
Space
Sleep 1800ms

# `n` opens an inline field — the `input` node kind, which nothing bundled draws.
Type "n"
Sleep 1200ms
Type "rebase the wsl branch"
Sleep 1600ms
Enter
Sleep 1800ms

# --- The top pane ------------------------------------------------------------
# This one asks to RUN A PROGRAM, so it is granted nothing until the file is
# trusted (record.sh trusts it, as a person would from settings → Interface →
# `t`). CPU, memory and load are parsed out of `top`; `r` reads it again now
# rather than waiting for the answer to go stale.
Ctrl+W
Sleep 2000ms
Type "r"
Sleep 2500ms

# --- Search ------------------------------------------------------------------
# Matches session names, agents, branches and repos — and the text on each
# agent's screen, which is the half that finds a session by the error in it.
Ctrl+A
Sleep 1200ms
Type "osc"
Sleep 2200ms
Escape
Sleep 1500ms

# Final beat on the whole arrangement, then quit (detaches sessions).
Sleep 1800ms
Ctrl+Q
Sleep 1s
