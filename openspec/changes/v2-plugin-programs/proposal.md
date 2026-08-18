## Why

A plugin can read anything and draw anything, and it can ask for a program's
*output* — but it cannot give you a program to **use**. `thurbox-doom` is the
sharp version of the request: a pane that runs `doom`, takes your keystrokes, and
resizes with its rect. So are the ordinary versions — `htop`, `lazygit`, a REPL, a
log tail you page through.

Two things stand in the way, and both are hardcoded rather than principled:

- **`Command::Shell { session }` is the only way a plugin gets a live pane**, and
  it always spawns `default_shell()`. There is no program argument.
- **That pane belongs to a session** (`ensure_shell_pane`, persisted per session),
  so a plugin cannot own one.

Everything downstream already exists and is proven by the companion shell: spawn
into a tmux window, parse with `vt100`, paint into the pane's rect, forward
keystrokes, propagate resize, re-adopt by pane id after a restart. The surface is
already addressed by a **kernel-interpreted string** (`<id>#shell`) rather than
strictly a session id, which is the seam this change widens.

## What Changes

- **A plugin can ask for a program and get a pane.** It names the pane; the
  kernel keeps it running and paints it. Idempotent, like `shell`: asking every
  frame is safe, which is the pattern `run` already established.
- **The pane belongs to the plugin, not a session.** One `doom`, whatever is
  selected. A session-scoped program would mean one doom per session, which is
  not what "a dedicated pane" means — and the session's own shell already covers
  "a terminal in this worktree, on this host".
- **A new `surface` source addresses it.** No new node kind: `surface` already
  resolves a kernel-interpreted id, and this is a second thing that id can name.
- **Keystrokes reach it.** Today a pane declaring `input = "session"` has its
  unclaimed keys forwarded to `App::focused_session`. That becomes "forwarded to
  whatever the focused pane's surface names", so the routing follows the tree the
  plugin returned rather than a session the kernel guessed at.
- **A new capability, `program`.** Deliberately *not* folded into `run`.
  `run` is bounded on every axis that matters — 256 KB of output, a 600 s
  timeout, four at a time — and an interactive program has none of those by
  design. Someone who trusted a pane to poll `top` every few seconds did not
  agree to "may hold a process open indefinitely and feed it my keystrokes".
  Widening an existing grant silently is the one thing the trust model exists to
  prevent.
- **Bounds of its own**, since `run`'s do not transfer: how many panes one plugin
  may hold, and what happens when it asks for one more.
- **Lifecycle stated rather than inherited.** What a `F10` reload does to a
  running pane, what quitting does, and what happens when the program exits on
  its own. The shell's answer — the window outlives the interface and is
  re-adopted by pane id — is the precedent, and it is a *decision* here rather
  than an accident of where the code lives.

## Capabilities

### New Capabilities

- `plugin-programs`: a plugin asking for an interactive program in a pane it
  owns — how the pane is addressed and painted, how keys and resize reach it,
  what its lifecycle is across reload and restart, how many a plugin may hold,
  and what granting the capability means.

### Modified Capabilities

None. Two requirements were considered and deliberately left alone:

- `plugin-authoring`'s **"What is loaded is listable without a terminal"** would
  be the natural home for "and what it asks to be able to do". It is already
  being modified by the unarchived `v2-plugin-packages`, and a second delta on
  one requirement has to restate the first's content — a merge hazard for no
  gain. The reporting requirement lives in `plugin-programs` instead, next to the
  capability it reports.
- `plugin-authoring`'s **verification** requirement is untouched: a pane holding
  a program is a pane like any other as far as loading and placement go.

## Impact

- **`src/kernel/command.rs`** — a `Program` command beside `Shell`, carrying the
  pane's name, the program and its arguments.
- **`src/kernel/node.rs` / `convert.rs`** — `surface` gains the spelling that
  names a plugin-owned pane. Four node kinds, still (rule 1).
- **`src/kernel/terminal.rs`** — the panes themselves. `render_session` is the
  paint seam and `output_stamp` the redraw signal; both already accept a
  kernel-interpreted id, so both grow a case rather than a parallel path.
- **`src/agent/backend.rs`** — `ensure_shell_pane` hardcodes
  `default_shell()`; the spawn+wire path underneath it (`Session::wire_up`,
  `ShellPane`) is what a program pane needs, and is reached the same way.
- **`src/kernel/host.rs`** — `Capability::Program`, declared as data like
  `Run`, so the inventory can say which files ask for it without reading them.
- **`src/main.rs`** — key forwarding stops assuming the target is a session.
- **`src/kernel/modals/interface.rs`** — a file asking for `program` says so.
- **`ui-plugins/`** — `doom` as the worked example, since a capability nobody
  can see used is one nobody trusts.
- **Docs** — `docs/PLUGINS.md`, `ui/README.md`, `thurbox.yml` (the plugin sandbox
  selene lints against), `CLAUDE.md`, and the website's interface page.
- **No schema change.** A plugin's pane is runtime state, not a persisted row.
  Whether the *pane id* is persisted is a design question, and the shell's
  precedent for it (`shell_backend_id`) is a session column that has no analogue
  for a plugin — see design.
