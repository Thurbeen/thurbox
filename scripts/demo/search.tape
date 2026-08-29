# VHS tape for the Thurbox search demo.
#
# Do NOT run this directly — run `scripts/demo/record.sh search`, which builds the
# dev binary, sets up an isolated environment, seeds one session per real agent CLI,
# installs the `examples/` demo interface, and exports $THURBOX_BIN.
#
# Requires: vhs (charmbracelet/vhs) + ffmpeg + ttyd. Search really opens on Ctrl+/
# (plus the Ctrl+7 and Ctrl+_ encodings the kernel folds into it), none of which
# arrive reliably through VHS+ttyd — record.sh rebinds it to Ctrl+A for the
# recording. The strip is full-width above the chrome bands and highlights matches
# in the pane that owns them rather than reprinting them.
#
# Every tape in one `record.sh` run shares the sandbox and the tmux session, so a
# tape that mis-drives the interface leaves its keystrokes in an agent pane for the
# NEXT clip to record. That is how a stray `w` and an `add-plugin-docs` prompt ended
# up in this clip: they came from session-creation.tape selecting the wrong
# repository. Check a frame after changing any tape, not just this one.

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

Sleep 2500ms

# Get OFF the terminal before opening search, and do it by NAMING a pane rather
# than by stepping the focus ring.
#
# Two traps meet here. `search.open` really is `Ctrl+/`, which is deliberately not a
# bare `Ctrl+<letter>` so it reaches search even from a focused agent — but these
# recordings rebind it to `Ctrl+A`, which IS one, so from a focused terminal it is
# passed through to the agent's PTY and search never opens. And `Ctrl+H` (focus
# previous) used to be a reliable way to the session list when the ring held two
# panes; the demo interface makes it four, so where one press lands is no longer
# something a tape can assume.
#
# `Ctrl+E` is `tasks.open` (rebound from F5), which issues an explicit focus
# command. That is deterministic whatever the ring contains.
Ctrl+E
Sleep 1400ms

# Open search (Ctrl+A here; really Ctrl+/). A non-modal strip docks above the
# chrome bands. ONE scope has a pane today — sessions — and a result carries the
# pane it belongs to, so search grows a scope whenever a pane returns. v1 also
# searched tasks, automations and files; this does not, and the queries below are
# chosen for what it does search.
Ctrl+A
Sleep 1600ms

# Type a query. Matches highlight live IN THE SESSION LIST — matching rows
# accented and underlined, the rest dimmed — rather than being reprinted in the
# strip, so the strip points at the thing it found instead of duplicating it.
#
# `osc` matches the `fix-osc52-tmux` session by name. It used to be `tri`, aimed at
# a TASK ("Triage flaky WSL host discovery") back when tasks were a search scope;
# with sessions the only scope, that query found nothing and the clip showed an
# empty strip.
Type "osc"
Sleep 2200ms

# Move the selection through the grouped results; the owning panel previews the
# selected row (its cursor moves) while focus stays in the strip.
Down
Sleep 1000ms
Down
Sleep 1000ms
Down
Sleep 1200ms

# Refine it. Counts and highlights update on every keystroke, and the scan of each
# agent's SCREEN kicks in after a short debounce — that half is what finds a
# session by the error printed in it rather than by what it was called.
#
# `host` matches the `add-wsl-host-tests` session by name and `docs-remote-hooks`
# by nothing at all, so the dimming is visible on the rows that did not match.
# Keep it in step with demo_session_name() in record.sh.
Backspace
Backspace
Backspace
Sleep 600ms
Type "host"
Sleep 2000ms

# Enter jumps to the selected result and focuses its pane (here, a session's
# terminal). Esc would instead restore the exact pre-search state.
Enter
Sleep 2200ms

# Off the terminal again before reopening. `Enter` above jumped to the result and
# focused that session's TERMINAL, so a second `Ctrl+A` from here would be passed
# through to the agent exactly as the first one was — which is why this clip used to
# end early with nothing but an agent on screen.
Ctrl+E
Sleep 900ms

# Open it once more and cancel with Esc, which puts back exactly what was on
# screen before the search — the selection included. `wsl` matches a session; the
# old `test` was aimed at a task that search no longer looks at.
Ctrl+A
Sleep 1200ms
Type "wsl"
Sleep 1800ms
Down
Sleep 1200ms
Escape
Sleep 1600ms

# Final beat, then quit (detaches sessions).
Sleep 1200ms
Ctrl+Q
Sleep 1s
