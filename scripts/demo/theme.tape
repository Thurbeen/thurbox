# VHS tape — Thurbox theme switcher feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh theme`, which builds
# the dev binary, sets up the isolated env, seeds one session per real agent CLI
# in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+Y opens the theme picker; Down/Up move, Enter selects. (No F-keys.)
# Moving the selection LIVE-PREVIEWS the palette (the whole TUI recolors), so
# this clip walks through EVERY built-in theme to show the full range. The demo
# starts in `doom` (the last entry, re-applied by record.sh before each tape),
# so we step UP through the list: doom -> solarized-light -> gruvbox-light ->
# tokyo-night-day -> catppuccin-latte -> gruvbox-dark -> tokyo-night ->
# catppuccin-mocha -> default, alternating dark and light palettes.

Output docs/media/thurbox-theme.gif
Output docs/media/thurbox-theme.mp4

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

# Step UP through every built-in theme. Each move recolors the live TUI; pause
# on each so the palette is clearly visible.
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms
Up
Sleep 1100ms

# Now back down to a vibrant dark palette and commit it with Enter.
Down
Sleep 1000ms
Enter
Sleep 1800ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
