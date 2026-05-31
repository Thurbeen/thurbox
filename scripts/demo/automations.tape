# VHS tape for the Thurbox Automations feature demo.
#
# Do NOT run this directly — run `scripts/demo/record.sh automations`, which
# builds the dev binary, sets up an isolated environment, seeds one session per
# real agent CLI, and exports the env vars this tape relies on ($THURBOX_BIN).
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. VHS has no function-key
# commands, so this uses Ctrl-key bindings + Tab / arrows only.

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

# Focus the Automations pane. The TUI opens focused on the terminal, so one
# Ctrl+H (focus previous pane) lands on the always-present Automations pane.
Ctrl+H
Sleep 1800ms

# Create a new automation right from the pane — Ctrl+N, just like a new session.
Ctrl+N
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
Sleep 450ms
Right
Sleep 700ms

# Skip timezone, leave the Action on "send -> active session".
Tab
Sleep 700ms
Tab
Sleep 900ms

# The prompt that fires on schedule.
Tab
Sleep 600ms
Type "Triage failing CI and summarize blockers"
Sleep 1200ms

# Save — the automation lands in the pane with a live next-run countdown.
Enter
Sleep 2200ms

# It's fully actionable from the pane: toggle enabled off and back on.
Space
Sleep 1300ms
Space
Sleep 1500ms

# Final beat, then quit (detaches sessions).
Sleep 2s
Ctrl+Q
Sleep 1s
