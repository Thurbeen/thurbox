# VHS tape — Thurbox session-creation workflow feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh session-creation`,
# which builds the dev binary, sets up the isolated env, seeds one session per
# real agent CLI in a sample repo, seeds a `~/projects` folder of several repos,
# and exports $THURBOX_BIN.
#
# Ctrl+N opens the repo picker. Highlights: repos show by basename (not full
# path); Ctrl+P imports a parent folder whose git sub-dirs are listed beneath a
# collapsible header; then the usual name -> agent picker -> spawn flow.

Output docs/media/thurbox-session-creation.gif
Output docs/media/thurbox-session-creation.mp4

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

# Open the repo picker (Ctrl+N). The sample repo is already bookmarked (the
# seeded sessions registered it) and now shows by basename, not full path.
Ctrl+N
Sleep 2s

# Focus the path input (Tab) and type a PARENT folder containing several repos.
Tab
Sleep 700ms
Type "~/projects"
Sleep 1200ms

# Ctrl+P imports it as a parent: its git sub-dirs are listed (by basename),
# nested under a collapsible "▾ projects (parent)" header — and the folder is
# re-scanned on every open, so new repos appear automatically.
Ctrl+P
Sleep 2500ms

# Space on the header folds the tree (▸), then unfolds it (▾) again.
Space
Sleep 1400ms
Space
Sleep 1400ms

# Move to a child repo and select it (Down to move, Space to check).
Down
Sleep 700ms
Space
Sleep 1400ms

# Confirm the selection (Enter).
Enter
Sleep 1500ms

# Session-name prompt: type a name, then confirm.
Type "review-bot"
Sleep 1500ms
Enter
Sleep 1500ms

# Agent picker (shown because several agents are registered): choose one.
Sleep 1s
Down
Sleep 800ms
Down
Sleep 900ms
Enter
Sleep 1500ms

# The new session appears in the list and its agent boots.
Sleep 3s

# Final beat, then quit (detaches sessions).
Sleep 1s
Ctrl+Q
Sleep 1s
