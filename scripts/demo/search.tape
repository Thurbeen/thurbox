# VHS tape for the Thurbox Global Search feature demo (Ctrl+A).
#
# Do NOT run this directly — run `scripts/demo/record.sh search`, which builds
# the dev binary, sets up an isolated environment, seeds one session per real
# agent CLI plus a few tasks/automations to search over, and exports the env
# vars this tape relies on ($THURBOX_BIN).
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. Global search opens with
# Ctrl+A (a reliable chord on every terminal); the strip docks full-width along
# the bottom and highlights matches live in the panels themselves.

Output docs/media/search-demo.gif
Output docs/media/search-demo.mp4

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

# On launch the active session's TERMINAL is focused (session restore focuses
# it), and Ctrl+W (FocusTasks) is a terminal-passthrough chord — so from the
# terminal it would be sent to the agent's PTY instead of toggling the panel.
# Ctrl+H (focus previous pane) is a navigation chord that is never passed
# through, so it reliably moves focus to the session list first.
Sleep 2500ms
Ctrl+H
Sleep 600ms

# Sessions are pre-seeded, plus a few tasks and an automation to search across.
# Surface the tasks panel first so its matches highlight live during the search.
Ctrl+W
Sleep 1200ms
Escape
Sleep 800ms

# Open global search (Ctrl+A): a non-modal strip docks along the bottom and
# searches EVERY scope at once — sessions, tasks, automations, and files.
Ctrl+A
Sleep 1600ms

# Type a query. Matches highlight live in the panels themselves (accent + bold +
# underline on matching rows, the rest dimmed); the strip shows per-scope counts
# and a grouped result list.
Type "tri"
Sleep 2200ms

# Move the selection through the grouped results; the owning panel previews the
# selected row (its cursor moves) while focus stays in the strip.
Down
Sleep 1000ms
Down
Sleep 1000ms
Down
Sleep 1200ms

# Refine the query — counts and highlights update on every keystroke (the
# session buffer-content scan kicks in after a short debounce).
Backspace
Backspace
Backspace
Sleep 600ms
Type "claude"
Sleep 2000ms

# Enter jumps to the selected result and focuses its pane (here, a session's
# terminal). Esc would instead restore the exact pre-search state.
Enter
Sleep 2200ms

# Open it once more to show a task jump, then cancel with Esc (state restored).
Ctrl+A
Sleep 1200ms
Type "test"
Sleep 1800ms
Down
Sleep 1200ms
Escape
Sleep 1600ms

# Final beat, then quit (detaches sessions).
Sleep 1200ms
Ctrl+Q
Sleep 1s
