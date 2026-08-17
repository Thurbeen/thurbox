# v2-plugin-commands

## Why

A v2 plugin can draw anything, and can read only what the kernel already
decided to publish: sessions, tasks, automations, themes, diffs, metrics,
repository memory. Everything a user might actually want a pane to show about
*their* work — container health, a package script's output, a linter's verdict,
the branch's real state as `git status --porcelain` reports it — is unreachable,
because Lua here has no process, no filesystem and no network.

So "write your own pane" currently means "write your own pane over thurbox's own
data". The panes people describe wanting are panes over a **third-party
program's** output — often several at once, composed into one view. Without
this, the plugin API is extensible in form and closed in practice, and every
such pane has to be a change to the binary instead of a file someone drops in.

The narrow alternative already exists and is not enough: the shell pane
(`Ctrl+T`) gives a real terminal in the session's directory, on the session's
own host. That covers *watching* a program. It does not let a pane **derive**
anything — count the dirty files, badge the failing container, colour a row by
an exit status — because nothing can read what the shell printed.

## What Changes

- **New capability: a plugin can ask for a program to be run** and receive its
  output. The ask is a command (`command("run", …)`), the answer is a published
  read (`thurbox.runs`) — the existing split, so Lua still never blocks.
- Runs happen **on a worker**, in the session's working directory, and for a
  remote session **on that session's own host** — the same rule `sync`,
  `restart` and the diff now follow. A program that would report on a remote
  machine's containers has to run there to mean anything.
- Output is **captured and bounded**: stdout, stderr and exit status, each
  capped, with a wall-clock timeout, so a program that never exits or prints
  without end cannot take the interface with it.
- A run is **keyed and cached**, with an explicit staleness policy, so a pane
  can keep `git status` current by asking every frame without running `git`
  every frame.
- **Several programs at once, per pane.** Keys are independent, so one pane
  composes `docker compose ps` + `git status --porcelain` + `npm outdated` and
  renders all three, each arriving when it arrives. A pane must never be forced
  to wait on its slowest program to draw the ones that finished — that is what
  makes a composite pane possible rather than merely allowed.
- **Concurrency is bounded** — a fixed number of runs in flight, the rest
  **queued rather than dropped** — so a pane that asks for twenty programs is
  slow, not broken, and cannot fork twenty processes.
- **The capability is declared, and granted per plugin by trusting it.** A
  plugin that wants to run programs says so in its declaration, and gets nothing
  until the user trusts *that plugin*. Trust is granted and revoked where the
  interface's files are already listed, so the question is answered in front of
  the file it is about rather than as a global switch somewhere else.

  This is not a sandbox and does not pretend to be one. Once `docker` runs, it
  runs with the user's authority; thurbox cannot make a program it spawns safe.
  What it can do is make the decision **explicit, per plugin, visible and
  revocable** — the same bargain a shell profile or an editor plugin offers. A
  file dropped into the interface directory must not gain arbitrary execution
  *silently*; it may gain it when the user says so.
- **A complex plugin stays modular.** `require` already reaches any module
  inside the interface directory (`require("mine.docker")` →
  `mine/docker.lua`), so a substantial plugin is already allowed to be several
  files rather than one. That is currently incidental; this change makes it a
  held requirement, because a pane over three programs with a real widget in it
  is not a file anyone wants to keep in one piece.
- **A complex widget is composition, not new node kinds.** The four kinds stay
  four. What this change owes is the proof: a worked example that is a genuine
  composite — more than one program, output parsed rather than echoed, and a
  widget (a table with aligned columns and per-row state) built from the
  existing primitives and a shared module. If that example cannot be written
  cleanly, the gap is in `lib/`, and closing it is part of this change rather
  than a later discovery.
- The interface's own directory becomes a **realistic place to work**: the
  worked example ships in `docs/examples/`, and `thurbox-cli plugin new` keeps
  writing a pane that needs no capability at all.

## Capabilities

### New Capabilities

- `plugin-commands`: what a plugin may ask to be run, where it runs, what comes
  back, what is refused, and what the user controls.

### Modified Capabilities

- `plugin-authoring`: the authoring surface grows a capability that must be
  declared before it can be used; the starter/check tooling has to report a
  declaration the interface will refuse and whether the plugin is trusted; and a
  plugin spanning several modules becomes a held guarantee rather than an
  accident of how `require` happens to resolve.

## Impact

- **New**: `src/kernel/runs.rs` (the store and its worker pool), a `Run` verb on
  the command bus, `thurbox.runs` on the published read surface, a
  `capabilities` field in a plugin's declaration.
- **Changed**: `src/kernel/command.rs` (the new verb), `src/kernel/host.rs`
  (publish, declaration parsing), `src/bin/thurbox2.rs` (poll the store, serve
  the queue), `src/kernel/registry.rs` (carry declared capabilities and persist
  trust), `src/kernel/inventory.rs` + `src/kernel/modals/interface.rs` (show and
  grant trust where the files are already listed), `thurbox.yml` +
  `docs/PLUGINS.md` (the sandbox's published shape and its documentation).
- **Reused, not rebuilt**: `git::host_shell_c` / the host launcher already run a
  command line on a host; `session_ops::run_exec_command` already runs one
  locally for `AutomationAction::Exec`. The worker/poll shape is
  `kernel::diff`'s, and the bounded-cache shape is `kernel::repos`'s.
- **Security**: this is the first capability that lets interface files affect
  anything outside thurbox, and it is the reason the change needs a design
  document rather than a task list. The position taken is that the user owns the
  decision — thurbox cannot prevent a malicious plugin, only refuse to run one
  unasked.
- **Possibly changed**: `ui/lib/widgets.lua`, if writing the worked example
  shows the primitives cannot express an aligned, stateful table without every
  plugin re-deriving the arithmetic. Deliberately left as a *finding* of the
  work rather than a guess made before it.
- **Not in scope**: streaming output (a run completes, then reports; watching a
  live log is what the shell pane is for), interactive programs (no stdin), and
  running a program with no session to root it in.
