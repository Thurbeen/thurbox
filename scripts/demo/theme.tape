# VHS tape — Thurbox theme switcher feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh theme`, which builds
# the dev binary, sets up the isolated env, seeds one session per real agent CLI
# in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+Y opens the theme picker; j/k or Down/Up move, Enter selects.
# Moving the selection LIVE-PREVIEWS the palette — the whole interface recolors,
# chrome bands included — so this clip walks down the list to show the range.
#
# It steps DOWN from the top: the picker opens on the *active* theme, which on a
# fresh demo profile is `Default` (the first entry), so stepping up would go
# nowhere. 36 palettes are listed, grouped under Dark and Light headers.

Output media/thurbox-theme.gif
Output media/thurbox-theme.mp4

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

# Open the theme picker (Ctrl+Y). It pre-selects the active theme — `doom`,
# the last entry — so the list opens at the bottom.
Ctrl+Y
Sleep 1400ms

# Step DOWN through the dark palettes. Each move recolors the live interface;
# pause on each so the palette is clearly visible.
Down
Sleep 1100ms
Down
Sleep 1100ms
Down
Sleep 1100ms
Down
Sleep 1100ms
Down
Sleep 1100ms
Down
Sleep 1100ms
Down
Sleep 1100ms

# Cross into the Light group, so the clip shows both halves of the range.
Type "G"
Sleep 1600ms
Up
Sleep 1200ms
Up
Sleep 1200ms

# Back to a vibrant dark palette and commit it with Enter.
Type "g"
Sleep 1000ms
Down
Sleep 900ms
Down
Sleep 1000ms
Enter
Sleep 1800ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
