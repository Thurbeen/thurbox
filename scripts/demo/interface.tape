# VHS tape — the interface is a directory you can edit.
#
# Do NOT run directly — run `scripts/demo/record.sh interface`, which builds the
# dev binary, sets up the isolated env, seeds one session per real agent CLI in a
# sample repo, and exports $THURBOX_BIN.
#
# This is the clip that shows what makes thurbox thurbox: every pane is a Lua
# file, and the Interface tab is the interface looking at itself. It walks the
# inventory (which files exist, where each came from, which are on screen), turns
# one off and watches it stop drawing, then turns it back on.
#
# Chords: `record.sh` seeds a ui.json rebinding `settings.open` to Ctrl+B,
# because the real chords are F6 and Ctrl+, — VHS can send neither. `]` moves to
# the Interface tab, `space` turns a file off, `Esc` closes.

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

# Let the real agent CLIs finish booting so the panes have live content.
Sleep 2500ms

# Open settings (rebound to Ctrl+B for the recording) and cross to the
# Interface tab with `]`.
Ctrl+B
Sleep 1400ms
Type "]"
Sleep 1800ms

# One `j` down to `plugins/10_sessions.lua` — the session list itself. The list
# does NOT open on it: row 0 is `65_search.lua`, whose slot is empty while search
# is closed, so toggling that shows no reflow at all. Movement is `j`/`k` here, as
# the modal's own hint line says; arrows are not bound for up.
Type "j"
Sleep 900ms

# `space` turns it OFF. Not deleted — the file stays on disk, intact; it is simply
# not loaded, so it declares no keys and occupies no slot.
Space
Sleep 1800ms

# Close the modal: the session list is no longer drawn. Its COLUMN is still
# reserved, because `layout.lua` declares that slot on `panels.shown("sessions")`
# — the F9 toggle — and knows nothing about the disabled set. Closing the
# arrangement up is a second edit. Recorded as a blocker in the UI review.
Escape
Sleep 3s

# Back in, and turn it on again the same way.
Ctrl+B
Sleep 1200ms
Type "]"
Sleep 1200ms
Type "j"
Sleep 700ms
Space
Sleep 1800ms

# Now walk the inventory with `j`. Every row is a real file: the bundled panes and
# the shared libraries, each with where it came from (bundled / edited / yours) and
# whether it is on screen right now.
Type "j"
Sleep 700ms
Type "j"
Sleep 700ms
Type "j"
Sleep 700ms
Type "j"
Sleep 1200ms

Escape
Sleep 2400ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
