# VHS tape — Thurbox session-creation workflow feature clip.
#
# Do NOT run directly — run `scripts/demo/record.sh session-creation`,
# which builds the dev binary, sets up the isolated env, seeds one session per
# real agent CLI in a sample repo (which bookmarks it), seeds a `~/projects`
# folder of several repos, and exports $THURBOX_BIN.
#
# Ctrl+N opens the two-pane repo picker: a filesystem BROWSER (left) feeds a
# BASKET of chosen repos (right). Highlights: `g` jumps to a typed path, repos
# show by basename marked `●`, recently-used repos are pinned as `★` favorites,
# `a` adds the highlighted dir to the basket, then Tab → basket → Enter confirms,
# followed by the usual name -> agent picker -> spawn flow.

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

# Open the repo picker (Ctrl+N): the browser (left) + an empty basket (right).
Ctrl+N
Sleep 2s

# `g` opens the "go to path" input; type a folder of several repos and Enter
# to jump the browser there.
Type "g"
Sleep 700ms
Type "~/projects"
Sleep 1200ms
Enter
Sleep 2s

# The browser now lists ~/projects: the recently-used sample repo is pinned at
# the top as a `★` favorite, then the folder's git repos (each marked `●`).
# The cursor starts on the first repo — `a` adds it to the basket.
Type "a"
Sleep 1400ms

# Scroll up (past the `..` row) onto the pinned `★` favorite and add it with one
# keystroke — the repo you keep spinning up sessions on is always one key away.
Type "k"
Sleep 500ms
Type "k"
Sleep 700ms
Type "a"
Sleep 1400ms

# Tab over to the basket to review the two chosen repos, then Enter confirms.
Tab
Sleep 1600ms
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
