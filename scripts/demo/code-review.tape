# VHS tape for the Thurbox native code-review feature demo.
#
# Do NOT run this directly — run `scripts/demo/record.sh code-review`, which
# builds the dev binary, sets up an isolated environment, seeds a worktree
# session ("review") whose branch has a real committed diff, and exports the env
# vars this tape relies on ($THURBOX_BIN).
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. VHS has no function-key
# commands, so the review toggle (F7 by default) is rebound to Ctrl+X by the
# runner's keybindings.json; everything else is Ctrl chords + plain keys.

Output docs/media/code-review-demo.gif
Output docs/media/code-review-demo.mp4

Set Shell "bash"
Set FontSize 18
Set Width 1920
Set Height 1080
Set Padding 16
Set Theme "Catppuccin Mocha"
Set PlaybackSpeed 1.0

# Launch the dev binary (isolated thurbox-dev socket + XDG dirs from the runner).
Hide
Type `exec "$THURBOX_BIN"`
Enter
Sleep 2s
Show

# The "review" session is seeded first, so it's selected on launch with its
# terminal focused. Ctrl+X (rebound ToggleReview) opens the native code-review
# view: a continuous diff of the branch (<base>..HEAD) in the centre, the
# changed-files list in the right column, an action bar across the bottom.
Sleep 2500ms
Ctrl+X
Sleep 2600ms

# Scroll the diff — added lines (green) and the file/hunk headers read clearly;
# the changed-files list tracks the current file.
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 1200ms

# `}` jumps to the next changed file; the files list highlight follows.
Type "}"
Sleep 1600ms

# `v` toggles side-by-side (old | new), then back to unified.
Type "v"
Sleep 2200ms
Type "v"
Sleep 1400ms

# Move onto a changed line, then comment: `c` opens an inline compose box; type
# a note, Tab cycles its classification (issue / suggestion / note / praise),
# Ctrl+S saves. The comment renders inline under its line with a coloured badge.
Type "j"
Sleep 400ms
Type "j"
Sleep 900ms
Type "c"
Sleep 1400ms
Type "Use checked_add to surface overflow"
Sleep 900ms
Tab
Sleep 700ms
Tab
Sleep 1000ms
Ctrl+S
Sleep 2200ms

# `r` marks the current file reviewed (a ✓ in the list + header).
Type "r"
Sleep 1600ms

# `t` opens the review-target picker: working changes, the whole branch, or a
# single commit — like tuicr's -w / -r / commit targets.
Type "t"
Sleep 1800ms
Type "j"
Sleep 1400ms
Escape
Sleep 1200ms

# Esc closes the review back to the agent terminal; final beat, then quit.
Escape
Sleep 1200ms
Ctrl+Q
Sleep 1s
