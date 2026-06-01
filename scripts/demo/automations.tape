# VHS tape for the Thurbox Automations feature demo.
#
# Do NOT run this directly — run `scripts/demo/record.sh automations`, which
# builds the dev binary, sets up an isolated environment, seeds one session per
# real agent CLI, and exports the env vars this tape relies on ($THURBOX_BIN).
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. VHS has no function-key
# commands, so this uses Ctrl-key bindings + j/k / Tab / arrows only.

Output docs/media/automations-demo.gif
Output docs/media/automations-demo.mp4

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

# Sessions are pre-seeded; the always-present Automations pane sits beneath the
# session list in the left column.
Sleep 2500ms

# The Automations pane is part of the left column: focus the session list
# (Ctrl+H from the terminal), then press `j` down past the last session to flow
# straight into the Automations pane — one continuous list.
Ctrl+H
Sleep 1200ms
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 1600ms

# Create a new automation right from the pane (`n`, like Ctrl+N for a session).
# The editor opens in the central pane — one screen, no separate popup.
Type "n"
Sleep 1800ms

# Name it.
Type "morning-triage"
Sleep 900ms

# Trigger is a selector cycled with the arrows — step through, land on "daily".
Tab
Sleep 700ms
Right
Sleep 800ms
Right
Sleep 800ms
Left
Sleep 800ms
Left
Sleep 900ms

# Hour stepper (arrows adjust, wrapping); watch the "next:" preview update.
Tab
Sleep 600ms
Right
Sleep 500ms
Right
Sleep 700ms

# Minute stepper.
Tab
Sleep 600ms
Right
Sleep 450ms
Right
Sleep 700ms

# Skip timezone, leave the Action on "send".
Tab
Sleep 700ms
Tab
Sleep 900ms

# Target selector — cycle through the running sessions to pick where the
# prompt is sent.
Tab
Sleep 700ms
Right
Sleep 800ms
Right
Sleep 900ms

# The prompt that fires on schedule.
Tab
Sleep 600ms
Type "Triage failing CI and summarize blockers"
Sleep 1200ms

# Save — returns to the pane; the automation shows a live next-run countdown.
Enter
Sleep 2000ms

# Toggle enabled off and back on, straight from the pane.
Space
Sleep 1100ms
Space
Sleep 1400ms

# Step into the central pane: Ctrl+L focuses the editor (its fields), again
# focuses the run-history panel beneath it.
Ctrl+L
Sleep 1600ms
Ctrl+L
Sleep 1600ms

# Run it now from the history panel (`r`); the run is recorded and the prompt
# is delivered to the target session.
Type "r"
Sleep 2500ms

# Ctrl+H walks back to the editor, then to the Automations list (like Esc) —
# the cycle stays within the automation context, never jumping to a session.
Ctrl+H
Sleep 1000ms
Ctrl+H
Sleep 1600ms

# Final beat, then quit (detaches sessions).
Sleep 1500ms
Ctrl+Q
Sleep 1s
