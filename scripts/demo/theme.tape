# VHS tape — Thurbox theme switcher feature clip.
#
# Do NOT run directly — run `scripts/demo/record-agents.sh theme`, which builds
# the dev binary, sets up the isolated env, seeds one session per real agent CLI
# in a sample repo, and exports $THURBOX_BIN.
#
# Ctrl+Y opens the theme picker; Down/Up move, Enter selects. (No F-keys.)

Output docs/media/thurbox-theme.gif
Output docs/media/thurbox-theme.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

# Launch the dev binary (isolated env from record-agents.sh).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# Let the real agent CLIs finish booting.
Sleep 2500ms

# Open the theme picker (Ctrl+Y) and step through palettes — the TUI recolors live.
Ctrl+Y
Sleep 1200ms
Down
Sleep 900ms
Down
Sleep 900ms
Down
Sleep 900ms
Down
Sleep 900ms
Enter
Sleep 1800ms

# Reopen and switch to a light palette to show the full range.
Ctrl+Y
Sleep 1s
Down
Sleep 900ms
Down
Sleep 900ms
Down
Sleep 900ms
Enter
Sleep 1800ms

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
