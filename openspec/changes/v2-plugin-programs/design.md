## Context

The companion shell already does almost all of this. A session lazily spawns
`$SHELL` into its own tmux window (`tbs-<name>`), the pane is wired to a
`vt100::Parser` with a reader task and a writer channel (`Session::wire_up` →
`ShellPane`), it is painted by `Terminals::render_session`, matched to its rect on
every paint, fed keystrokes, and re-adopted by pane id after a restart. Two things
about it are hardcoded rather than principled: `ensure_shell_pane` calls
`self.backend.default_shell()` with no way to name a program, and the pane hangs
off a `Session`.

Three seams already generalise further than they are used, which is what makes this
change small in shape rather than a second terminal stack:

- **The surface id is interpreted, not opaque.** `SurfaceSource::Session(String)`
  carries a string the kernel resolves, and `<id>#shell` already means "that
  session's other pane". A third meaning is a case, not a new node kind (rule 1
  holds: still four).
- **Key routing already follows the returned tree.** `App::focused_session` is set
  from `node.first_session_surface()` — a walk of the focused plugin's *own output*
  — and `handle_key` forwards unclaimed keys there. The kernel genuinely does not
  know which plugin "is" the terminal. What is narrow is only the set of things a
  surface may name.
- **Window names are the identity.** `sanitize_window_name` is deterministic and
  the doc comment already says callers must use it at both creation and lookup;
  `Terminals::discovered` maps `tb-<name>` → pane id by listing windows. A
  deterministically named window is re-findable with no stored state at all.

The constraint that shapes everything: **Lua never blocks** (rule 3) and anything
touching the world runs on a worker (rule 5). Starting a program is a `command`,
which is already accepted-now-and-surfaced-later; reading its screen is a snapshot
read of a grid some other thread fills.

## Goals / Non-Goals

**Goals:**

- A plugin can put an interactive program in a pane it owns, and `thurbox-doom` is
  a plugin somebody can actually write.
- Keystrokes, resize and repaint work the way they already do for a session's
  terminal, through the same code rather than beside it.
- The capability is granted separately from `run`, and what a file is asking for is
  legible before granting it.
- The lifetime is a stated decision at every edge: reload, quit, plugin removed,
  program exits.
- A plugin's pane can never be mistaken for a session, in any surface that
  enumerates them.

**Non-Goals:**

- **A plugin's pane on a remote host.** The backend registry makes it reachable and
  the design leaves room, but "which host does a plugin-owned pane run on" is a
  question with no obvious answer once it is not a session's pane, and answering it
  badly is worse than not answering. Local only; see Open Questions.
- **Scrollback, selection, copy, hyperlinks in a program pane.** These exist for
  session surfaces and are per-surface policy the pane can grow later. A game and a
  REPL want opposite answers, which is a reason to defer rather than to guess.
- **Persisting a pane across an interface restart as a *record*.** Re-finding it by
  window name is the mechanism (D5), which needs no row.
- **Letting a plugin address another plugin's pane.** Made impossible by
  construction (D3), not by a check.
- **A second capability model.** Trust stays per file, keyed as it already is.

## Decisions

### D1 — The pane is the plugin's, keyed by (plugin, name)

A plugin asks for `name`, and the kernel keys the pane on the **calling plugin's
path plus that name**. Not on a session, per the user's decision: one `doom`,
whatever is selected.

The plugin's path is the key because it is already the identity everything else
about a file uses — trust, the disabled set, `run`'s attribution, the inventory.
Using the declared `name` instead would let two files claim one pane by declaring
the same name, and would move a pane when its author renamed the plugin.

*Alternative.* Key on the session, reusing `ensure_shell_pane` almost verbatim —
near-free, and right for `htop`-on-this-host. Rejected as the user's call and
because the session's own shell already covers "a terminal in this worktree".

### D2 — State lives in `Terminals`, beside `live`

A `programs: HashMap<String, ProgramPane>` on `Terminals`, where `ProgramPane` is
`ShellPane`'s shape (parser, input channel, backend id, exited flag, output stamp).

Not a new `kernel::programs` module, which was the first instinct. Everything a
pane needs is already a method on `Terminals`: the `BackendRegistry` (local plus
every host), the `SurfaceProvider` impl that paints, `output_stamp` that signals a
redraw, `send` that writes keystrokes, `forget_rects`/`last_rect` for hit-testing.
A separate module would either duplicate that access or need `Terminals` passed
into it everywhere, and — decisively — `SurfaceProvider` has **one** implementor by
design, so a second provider is not available as an option.

The cost is real: `terminal.rs` is already ~1300 lines. Accepted, because the
alternative splits one paint seam in two.

### D3 — The surface names the pane, and the kernel stamps the owner

In Lua: `{ type = "surface", program = "doom" }`. The plugin never writes an owner
and cannot name another plugin's pane — the kernel resolves `program` against the
plugin currently being rendered, the same way `state` is namespaced by plugin and
`run`'s answers are namespaced by asker.

This is the property worth engineering: cross-plugin access is impossible *by
construction* rather than refused by a check that could be forgotten. It is the
same lesson as `RUN_IMPL` living in the VM registry rather than in globals — a
name in reach is a name that gets reached.

`SurfaceSource` gains `Program { name: String }`, resolved at paint time to the
`(plugin, name)` key. `first_session_surface` becomes a walk for the first *live*
surface of either kind, which is what makes key routing (D4) fall out.

*Alternative.* A separate node kind. Rejected outright: four node kinds, forever.

### D4 — Raw input declares intent; the tree decides the target

`plugin.session_input` (declared `input = "session"`) already means "forward keys I
did not handle". Today the target is `App::focused_session`, derived from the
focused plugin's tree. Generalise the *target*, not the declaration: the walk
returns whichever surface it finds, and the key goes there.

`input = "session"` keeps working unchanged — it is what `20_agent.lua` declares —
and now reads as "this pane wants raw input", which is what it always meant. No new
spelling to learn and no migration.

The escape-route rule is unchanged and load-bearing: `RESERVED` chords and the
navigation/quit set are never deferred, so a program that eats every key cannot
trap the user. This is why `doom` is safe to run at all.

### D5 — Re-adoption by window name, not by a persisted id

The window is `tbp-<digest of plugin path>-<name>`, deterministic, and found again
by listing windows — the mechanism `Terminals::discovered` already implements for
agent windows.

The shell persists `shell_backend_id` **on the session row**. A plugin's pane has
no row, and inventing one (a `metadata` key per plugin per pane) would be
persistent state that can disagree with reality — the class of bug `docs/CONFIG.md`
reserves SQLite for and that this repository has already been bitten by ("we have
an answer" stored where "the answer is current" was needed). A deterministic name
cannot go stale: either the window is there or it is not.

The plugin path is **digested** rather than sanitized into the name because
`sanitize_window_name` maps every non-`[A-Za-z0-9_-]` character to `_`, so
`plugins/90_doom.lua` and `plugins.90.doom.lua` would collide — and a path is long
enough to make window names unreadable.

*Alternative.* Kill the pane on quit, so nothing is ever re-adopted. Simpler and no
orphans — and it throws away the game you were playing, which is the whole feature.

### D6 — Lifetime: reload keeps it, a vanished plugin releases it

- **Reload (`F10`)** keeps the program running, for free: `reload_interface`
  rebuilds `host` and `Terminals` is a separate field. Worth asserting rather than
  relying on, since it is the edge that would make the feature unusable — you
  reload after every edit.
- **A plugin that is gone** — deleted, renamed, disabled — has its panes released,
  following `runs::retain_plugins(live)`, which `reload_interface` already calls for
  exactly this reason ("must not leave its answers behind to accumulate across
  reloads"). Released means the window is killed: an invisible unreachable `doom` is
  worse than a closed one, and unlike a session there is nothing in the interface
  that could ever show it again.
- **Quit** detaches, as it does for sessions. The window survives and D5 finds it
  next launch.
- **The program exits** on its own: the pane reports it (the `exited` flag already
  exists on `ShellPane` and the reader loop already sets it) rather than painting a
  frozen grid, and asking again starts it afresh.

### D7 — `Capability::Program`, with its own bound

A second enum case, declared as data like `Run`, so the inventory can say which
files ask for it without reading them — and so a third capability is a compile
error at every site that decided about the first two, which is why the enum exists.

The bound is **four panes per plugin**, matching `run`'s `MAX_CONCURRENT` so there
is one number to remember rather than two. Refusal is visible to the plugin (the
means of asking reports it) so a pane can say why it has nothing to show.

`run`'s other bounds deliberately do **not** transfer, and that is the whole
argument for a separate capability: `OUTPUT_CAP` is meaningless for a screen that
is overwritten in place, and `MAX_TIMEOUT` is the opposite of what an interactive
program wants. There is nothing left to bound except how many.

### D8 — A pane is not a session, and it is kept out of the session tables

Sessions are enumerated from the **snapshot**, which is built from SQLite rows.
A program pane is never a row, so it cannot leak into the session list, the
count, the status derivation, or `thurbox-cli session list`. This is a property of
where the state lives, not a filter — but it is asserted, because the machinery
underneath is shared and a leak would be a session the user cannot delete or
explain.

The window prefix is distinct (`tbp-`) so window *discovery* does not adopt one as
an agent pane either.

### D9 — `thurbox.yml` needs no new global

Asking goes through the existing `command` global, and the capability is a field in
the declaration table. So the plugin sandbox's selene standard library is unchanged
— nothing new is in reach, which is the point of granting by absence. `thurbox.yml`
does gain nothing, and that fact is worth stating because "a newly published field
used by a plugin fails lint until it is added" is a rule this repository enforces.

### D10 — `thurbox.granted`, because absence cannot express this one

Implementation found a gap the design had not: **rule 4 does not reach this
capability.** `run` is withheld by *absence* — it is simply not a function until the
file is trusted, which is the check `docs/examples/composite.lua` makes and the
whole of how a pane draws an honest "you have not trusted me". A program pane is
asked for through `command`, which every plugin has, so there is nothing to be
absent. Without an answer, a pane could not tell "not trusted" from "still
starting", and would draw an empty box in both cases — the exact failure the
capability model is otherwise good at avoiding.

So `enter` publishes `thurbox.granted.<name>` per plugin, beside `thurbox.runs`.
It grants nothing: it is a boolean about a decision the user already made, and it is
declared in `thurbox.yml` so a plugin reaching for it lints. The spec scenario that
said the means of asking would be "absent rather than present and failing" was
written for `run`'s shape and is corrected to match: the *effect* is withheld, and
the plugin can see that it is.

### D11 — Re-adoption needs its own lookup, because `discover` filters to `tb-`

Also found in implementation, and it would have shipped silently broken. D5 said the
window is found again by listing windows — but `discover()` filters on
`WINDOW_PREFIX` (`tb-`), and `tbp-` does not match it. The re-adoption path would
never have seen its own window.

The same filter is why the companion shell persists a pane id: `tbs-` fails it too
(the comment at that filter claiming it covers shells is simply wrong). So
`SessionBackend` gains `find_window(name)` — the same listing, unfiltered and matched
exactly rather than by tmux's FNMATCH-ish prefix rule. The filter is a *feature* in
the other direction: it is what stops a plugin's pane ever being adopted as a
session's agent, which D8 asserts.

## Risks / Trade-offs

- **A plugin can now run an arbitrary program with the user's authority, and hold
  it open** → It could already run one via `run`; what is new is duration and
  stdin. Three things stand between a package and this and none is weakened: the
  capability is absent until granted, the grant is per file and prompted, and it is
  a *different* grant from `run` precisely so it cannot be inherited. Stated
  plainly: this is not a sandbox, and thurbox's position is that it can only refuse
  to run things unasked.
- **`terminal.rs` grows** (D2) → Accepted deliberately over splitting the paint
  seam. Mitigated by keeping `ProgramPane` the same shape as `ShellPane` so the two
  read as one pattern rather than two.
- **A program that ignores resize draws wrong** → Nothing to be done: we send the
  size, the program decides. The pane is at least *born* at its rect size, which is
  the bug the shell hit (`open_shell` notes it: born a screen wide because the memo
  looked settled).
- **An orphaned `tbp-` window** if a plugin is deleted while thurbox is not running
  → D6 releases panes on reload, which covers the running case; the not-running
  case leaves a window that the next launch does not adopt (no plugin asks for it)
  and does not kill (nothing knows it should). A documented leave-behind, exactly
  as remote hook provisioning already has one. Worth a `plugin` CLI escape hatch
  later; not worth inventing persistence for.
- **Keys reaching a program the user thinks is inert** → The forwarding is gated on
  the pane having focus, and the escape chords are never deferred. The failure mode
  to avoid is a *silently* swallowed key, which is why an unbacked pane must not
  report the key as handled (a spec scenario).
- **Two plugins both wanting `doom`** → They get two panes, two windows, two
  processes, by D1's key. Correct, if surprising; the alternative (sharing by name)
  is the cross-plugin access D3 exists to prevent.

## Migration Plan

Nothing to migrate. No schema change, no config change, no change to any existing
plugin: `input = "session"` keeps its meaning (D4), `SurfaceSource::Session` is
untouched, and an interface with no plugin declaring `program` behaves exactly as
now. The capability is opt-in twice over — declared by a file, then granted by the
user.

Rollback is removing the capability from a plugin's declaration, or revoking the
trust; either leaves the pane unable to start and the plugin still loading.

## Open Questions

- **Should a plugin's pane be able to run on a remote host?** The backend registry
  makes it mechanically easy and `run` already goes remote for a session's host. The
  hard part is that a plugin-owned pane has no session and therefore no host, so the
  plugin would have to name one — which means plugins knowing about `hosts.toml`,
  which nothing in the interface does today. Deferred, not refused.
- **Should the pane's working directory be nameable?** Local-only makes the
  interface directory the obvious default, and `doom` does not care. A REPL or a log
  tail would. Related to the question above: both are really "how much context does a
  plugin-owned pane get to choose".
- **Does a program pane want scrollback?** The agent surface has it and the shell
  deliberately does not (`20_agent.lua`: a page key on the shell tab is left to the
  pty). A pane holding `less` wants the pty to have it; one holding a log tail might
  want ours. Left to whoever needs it, since guessing adds a knob nobody asked for.
