# VHS tape — the interface is a directory you can edit.
#
# Do NOT run directly — run `scripts/demo/record.sh interface`, which builds the
# dev binary, sets up the isolated env, seeds one session per real agent CLI,
# installs the `docs/examples/` demo (layout + tasks + top), trusts the pane that
# runs a program, and exports $THURBOX_BIN.
#
# This is the clip that shows what makes thurbox thurbox, and the demo interface is
# what makes it land: two of the four panes on screen came from `docs/examples/`,
# so the inventory lists them as **yours** next to the bundled ones, and
# `layout.lua` as **edited** because the demo replaced it. Those two words are the
# provenance column doing its job — a file you wrote and a shipped file you changed
# are both distinguishable from the set as delivered.
#
# The toggle is deliberately one of YOUR panes rather than a bundled one. Turning
# off the session list proves the arrangement closes up; turning off a pane you
# added proves the same machinery applies to a file thurbox never shipped, which is
# the actual claim.
#
# Chords: record.sh rebinds `settings.open` to Ctrl+B for the recording (really F6
# or Ctrl+, and VHS can send neither). `]` crosses to the Interface tab, `space`
# turns a file off, `Esc` closes.
#
# ROW ORDER matters here and is easy to get wrong. The tab sorts by state — what
# needs you first — so with the demo installed it is:
#
#   0  plugins/65_search.lua      no slot   (its strip is closed)
#   1  plugins/10_sessions.lua    on screen
#   2  plugins/20_agent.lua       on screen
#   3  plugins/80_tasks.lua       on screen   yours
#   4  plugins/85_top.lua         on screen   yours
#
# Movement is `j`/`k`, as the modal's own hint line says; arrows are not bound for
# up. Verify with `thurbox-cli plugin list` after changing anything here.

Output docs/media/thurbox-interface.gif
Output docs/media/thurbox-interface.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the agents boot and the `top` pane get its first answer, so the arrangement
# on screen is the finished one before anything is turned off.
Sleep 3500ms

# Open settings and cross to the Interface tab.
Ctrl+B
Sleep 1400ms
Type "]"
Sleep 2200ms

# Walk down to the two panes that came from `docs/examples/`. They are marked
# `yours`; the bundled ones above say nothing, because there is nothing to say.
Type "j"
Sleep 700ms
Type "j"
Sleep 700ms
Type "j"
Sleep 1400ms
Type "j"
Sleep 1800ms

# `space` turns `85_top.lua` OFF. Not deleted — the file stays on disk, intact; it
# is simply not loaded, so it declares no keys and is granted no capability.
Space
Sleep 1800ms

# Close the modal: the pane is gone AND its row in the right column with it, so
# the tasks pane takes the whole column. A slot no plugin can fill is not a rect
# reserved for nothing.
Escape
Sleep 3200ms

# Back in, and on again the same way.
Ctrl+B
Sleep 1200ms
Type "]"
Sleep 1000ms
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 700ms
Space
Sleep 2000ms

# Then keep walking. Every row is a real file: the bundled panes, the two that are
# `yours`, `layout.lua` marked `edited` because the demo replaced the shipped one,
# and the shared libraries under `lib/`.
Type "j"
Sleep 700ms
Type "j"
Sleep 700ms
Type "j"
Sleep 900ms
Type "j"
Sleep 1400ms

Escape
Sleep 2600ms

# Final beat on the restored arrangement, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
