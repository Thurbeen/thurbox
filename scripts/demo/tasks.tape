# VHS tape for the Thurbox Tasks (todo list) feature demo.
#
# Do NOT run this directly — run `scripts/demo/record.sh tasks`, which builds
# the dev binary, sets up an isolated environment, seeds one session per real
# agent CLI, and exports the env vars this tape relies on ($THURBOX_BIN).
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. VHS has no function-key
# commands, so this uses Ctrl-key bindings + j/k / Tab / arrows only (the tasks
# panel toggles with Ctrl+W, the F5 alternative).

Output docs/media/tasks-demo.gif
Output docs/media/tasks-demo.mp4

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

# Sessions are pre-seeded. Open AND focus the tasks panel — a toggleable
# right-side column between the terminal and the file viewer (Ctrl+W, the F5
# alternative). It starts empty ("no tasks").
Sleep 2500ms
Ctrl+W
Sleep 1800ms

# Create the first task right from the panel (`n`, like Ctrl+N for a session).
# The editor opens in the central pane — one screen, no separate popup.
Type "n"
Sleep 1500ms

# Title it.
Type "Add integration tests"
Sleep 1000ms

# Status is a selector cycled with the arrows: todo -> in progress -> done.
Tab
Sleep 700ms
Right
Sleep 900ms

# Action selector cycles Local -> Send -> Spawn. Leave it Local (a plain todo).
Tab
Sleep 1000ms

# Save — back to the panel; the new task shows with its ◐ in-progress checkbox.
Enter
Sleep 2000ms

# Add a second task, this time connected to an agent.
Type "n"
Sleep 1200ms
Type "Triage failing CI"
Sleep 1000ms

# Cycle the Action field to Send, so triggering pastes the title into a session.
Tab
Sleep 500ms
Tab
Sleep 600ms
Right
Sleep 1200ms

# Target selector — cycle through the running sessions to pick the destination.
Tab
Sleep 700ms
Right
Sleep 800ms
Right
Sleep 1000ms

# Save it.
Enter
Sleep 1800ms

# Back in the panel, j/k select; the action summary shows beneath the list.
Down
Sleep 1000ms
Up
Sleep 1200ms

# Space cycles the selected task's status in place (todo/in progress/done).
Space
Sleep 1100ms
Space
Sleep 1400ms

# Trigger the connected task now with `r`: its Send action pastes the title
# into the target session and the task advances to in-progress.
Down
Sleep 800ms
Type "r"
Sleep 2500ms

# Esc returns focus to the session list; Ctrl+W hides the panel again.
Escape
Sleep 1200ms
Ctrl+W
Sleep 1500ms

# Final beat, then quit (detaches sessions).
Sleep 1200ms
Ctrl+Q
Sleep 1s
