# VHS tape — Thurbox session-creation workflow feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh session-creation`,
# which builds the dev binary, sets up the isolated env, seeds one session per
# real agent CLI in a sample repo, seeds a `~/projects` folder of several repos,
# and exports $THURBOX_BIN.
#
# Ctrl+N opens the creation flow — one floating plugin rather than v1's chain of
# modals. Steps: host (skipped with no hosts.toml) -> repositories (multi-select,
# `w` per repo for a worktree) -> base branch -> name -> branch name -> agent.

Output media/thurbox-session-creation.gif
Output media/thurbox-session-creation.mp4

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

Sleep 2500ms

# Ctrl+N opens the creation flow. It is one floating plugin
# (ui/plugins/70_new_session.lua) rather than v1's chain of five modals, and it
# walks: repositories -> base branch -> name -> branch name -> agent. The host
# step is skipped here because no hosts.toml is seeded, so this is local.
Ctrl+N
Sleep 2200ms

# The repository step, and the row the cursor starts on is NOT the demo repo.
#
# The flow offers the INTERFACE DIRECTORY first — "Thurbox interface — edit your
# panes" — because editing a pane is work every user can do without cloning
# anything. So `space` here selected that instead: the session was created in
# `~/.config/thurbox-dev/ui`, the flow diverged (it is not a git repository, so `w`
# was refused), and the rest of this tape's keystrokes went into the agent's PTY.
# The damage outlived the clip, because every tape in one `record.sh` run shares the
# sandbox and the tmux session: `add-plugin-docs` was still sitting in the agent
# pane when search.tape recorded.
#
# `record.sh` seeds the demo repo as a bookmark so there is a second row at all —
# without that the picker offered ONE row, the interface directory, and `j` had
# nowhere to go. Row 0 is still the interface (it is always offered), so one `j`
# lands on the repo. If this looks wrong, check `repo_bookmarks` in the sandbox DB.
Type "j"
Sleep 900ms
Space
Sleep 1400ms

# `w` marks it worktree-backed: the session gets its own isolated checkout on a
# new branch rather than working in the repo directly.
Type "w"
Sleep 1600ms

# Confirm the repositories and move on.
Enter
Sleep 2200ms

# The base branch, fetched on a worker so the list is what the remote actually
# has. The default is pre-selected — take it.
Enter
Sleep 1800ms

# Name the session after the work, not the agent.
Type "add-plugin-docs"
Sleep 1200ms
Enter
Sleep 1600ms

# The branch name, pre-filled from the session name.
Enter
Sleep 1800ms

# The agent. Every entry comes from agents.toml, so this list is whatever CLIs
# are installed and declared.
Sleep 800ms
Down
Sleep 900ms
Enter
Sleep 3s

# The new session appears in the list, inside its repository group, and its agent
# boots in the pane. Final beat, then quit.
Sleep 2s
Ctrl+Q
Sleep 1s
