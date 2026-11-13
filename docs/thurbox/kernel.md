# The plugin kernel

thurbox is the interface to Fleet's sessions, with a Lua-driven renderer. The
kernel owns no pane: the session list, the terminal and every other surface that
shows *your work* is a plugin under `ui/`. The bundled set is currently
**three** — the session list, the agent pane and the search strip — plus four
floats that fill no slot: the new-session flow, the confirmation, the rename
field and the context menu. It does own the **system modals** — help, settings,
the theme picker and the interface's own file list — which are chrome about
thurbox itself rather than panes (see below). The file list is not a pane: it is
how a broken interface is recovered, so it must not be a file the interface
loads. Written for someone about to change the kernel; if you want to *write* a
plugin, read `docs/thurbox/plugins.md`.

Writing a plugin starts at `docs/thurbox/plugins.md` — **Start here**, which is four
`thurbox-cli plugin` commands and needs no terminal.

Code review and an info panel are panes rather than kernel features:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) (the
first consumer of `thurbox.diffs`) and
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) (drawn
entirely from the snapshot). Each is its own repository, installed by clone —
`docs/thurbox/plugins.md` has the commands.

## Shape

```text
   KERNEL (src/kernel/)                  LUA (ui/)
   ─────────────────────────             ──────────────────────────
   node      four primitives             layout.lua    arrangement
   layout    rects before render         lib/theme     roles
   convert   table <-> node              lib/ui        panel, list, cursor, row…
                                         lib/widgets   measure, window, list…
   paint     node -> ratatui             lib/tree      decoration helper
   host      VM, reload, isolation       plugins/*     3 panes + 5 floats
   registry  keys, settings, commands
   modals    help, settings, theme, files, palette
   events    derived from the snapshot
   snapshot  the read side
   command   the write side
   config    the user's settings, live and restart-only
   repos     what the creation flow asks
   updates   whether a newer release exists
   terminal  live PTY surfaces
   diff      review surfaces
   files     rooted file reads
   theme     36 palettes resolved
   notify    the blocked edge
   metrics   machine, agent
   perf      counters
   bundled   embedded interface, and what was done to your copy of it
   inventory which files exist, and which are running
```

## The five rules

**1. Four node kinds, forever.** `text`, `box`, `input`, `surface`. Everything
else composes in Lua — `ui/lib/ui.lua`, the component layer, over
`ui/lib/widgets.lua`, the primitive kit. A prior attempt froze its catalog at six
and reached sixteen, because it never built the userland layer and each new
appearance had nowhere else to live. `tests/kernel_mvp.rs` asserts the count.

**A pane's render is not guaranteed once per frame.** A pane that declares
`pure = true` asserts its render is a function of the published tables and its
render context, and the kernel then reuses the tree it last returned while both
stand — skipping the Lua call and the table→node conversion, which together were
the largest cost in a frame. Undeclared panes are called on every frame they are
drawn on, exactly as before. See `docs/thurbox/plugins.md` → Traps for what disqualifies
a pane, and ADR-P16 in `docs/thurbox/performance.md` for the measurements.

**2. Layout resolves before render.** Rects are computed first, then each plugin
is called with its own. A plugin that does not know its width cannot wrap,
truncate, or derive a scroll window — this was the single biggest gap the prior
attempt reported. The circularity is broken by plugins declaring their size
*statically*, in their declaration table, not in render output.

**3. Snapshot-read, command-write.** Reads are served from an in-memory snapshot
and return instantly. Writes are commands that are accepted and surface later.
Lua never blocks and never awaits, so no plugin — including ones nobody has
written — can stall the loop on Fleet, git, or an unreachable host.

**4. Capabilities by absence.** An ungranted capability is not in the
environment. Not blocked; absent. The file viewer needs file contents, so the
kernel reads them and the plugin draws — it does not get a filesystem. The
granted set is asserted by a test, so adding one is a deliberate edit.

**5. Anything that touches the world runs on a worker.** Attaching a terminal,
running a command, computing a diff, sampling metrics and answering the creation
flow's questions all do the same thing: work off the render path, publish the
result. When you add another, follow the pattern. (Attach is the one with the
sharpest teeth: it opens the backend's control-mode connection, so a host that is
down runs out its ssh timeout — inline, that was the whole interface frozen
before its first paint. It also needs the runtime *entered* on the worker, since
adopting a pane wires its reader and writer as tokio tasks.) (`kernel::metrics` is the
clearest to copy: `sysinfo` walks `/proc` and a pane's pid is a control-mode
round trip — one cadence, one published result. Account quotas are not among
them: a provider query belongs to a plugin granted `run`, not to the kernel.
`kernel::repos` is the newest, and adds the one wrinkle the others do not have:
its reads are *parameterised* — which directory, which repository — and a
plugin cannot call something that waits, so the flow asks by leaving a key in
`store` and the loop serves it.)

`kernel::worker_store::WorkerStore<K, V>` provides a reusable bounded pool,
deduplication, aged answers and a change generation. Its first user is
`files.read`: resolving a session-rooted path and reading its text both run on a
worker. A Lua call returns `nil` until ready and uses the cached answer during
refresh; changed completions invalidate pure panes. See `docs/thurbox/plugins.md` for
its bounds and error contract. The existing specialized stores keep their
scheduling and lifecycle contracts; they are not migrated here.

The command bus serializes commands naming the same non-empty session in
dispatch order. Each active session has one worker draining its queue; queued
commands stay `Queued` until execution starts. Other sessions, commands without
a session and housekeeping run independently, so an unreachable host holds up
only later commands for that session. Empty queues retire without waiting for
the render loop to poll completions.

## A capability that cannot be absent and useful

"Capabilities are absent rather than blocked" is easy while the answer is always
absent: there is no `os`, no `io`, no `package`, and `thurbox.yml` says so. The
first capability a plugin can be *granted* breaks that symmetry — it has to be
present for the plugin that may use it and absent for every other, in one Lua
state they all share.

**Each plugin runs in an environment of its own** (`host::api::plugin_env`).
Every plugin shares one Lua state, so "the plugin running now" cannot say who
issued a call: a pane that replaced a global another pane calls — `type`, say —
would run its own code inside that pane's call, holding its grant and stamped
with its path. So identity is bound where a function is made, never read from
who is running. A plugin's chunk gets its own `_ENV`, and `command`, `run`,
`files`, `state` and `require` are created for that plugin with its path bound
in. Code a plugin defines resolves those names through its own environment
wherever it is called from, so a hijacking function acts as its author and
nobody else.

Reads fall through to the shared globals, which hold only what every plugin may
have; writes land in the plugin's own table, so a global one plugin sets is
invisible to the next. `_G` is the plugin's own table and the environment's
metatable is sealed, so the shared globals are not reachable to write to. The
shared tables are reached only through views of the plugin's own: each plugin
gets an empty table per name over a sealed metatable. `string`, `table`, `math`,
`coroutine`, `utf8` and `text` read through and refuse a write. `store` is the
one that takes writes, because it is the bus between panes — a request left for
another pane, a selection the agent pane follows — and every plugin reads what
any plugin wrote. Only data crosses it: a write is converted to a stored value
(strings, numbers, booleans and tables of them; a function or a metatable is
dropped) and every read hands back a fresh copy, so one pane can tell another
something but cannot hand it code to run. A pane acting on a request it read
there issues it with its own `command`, under its own name: the shipped
confirmation forwards the one in `store.confirm` only after the user's keypress.
The views are per plugin because `rawset` writes past `__newindex` into the view
itself, and one view every plugin shared would carry that write to all of them.
Nothing on a view leads back to the table behind it — `__pairs` iterates through a
closure rather than returning the table as its state — and the string
metatable is sealed so `getmetatable("")` cannot hand one out. `require` has a
cache per plugin and runs a module in the requiring plugin's environment — a
shared module would be a table one plugin could rewrite under another, and a
module's `command(…)` has to act as the plugin that required it.

`thurbox` is the published data every plugin reads, so it is read-only all the
way down: each published group is frozen into read-only proxies when it is
built (`api::freeze`), the top-level table refuses a write, and `pairs` over
any of them hands back the proxy itself as the state, which the kernel's own
`next` reads through. Those proxies are shared, which is safe because `rawset`
— the one way to write past a metatable — is withheld. Their metamethods find
the table behind a proxy through a weak registry map and never capture it: a
captured table is a registry reference, a root the collector cannot see past, and
with one per proxy a rebuilt group took one full collection per nesting level to
free — a long deleted history filled the VM's ceiling within minutes. Each
plugin reads it through a `thurbox` view of its own, and what the kernel sets
per call (`runs`, `granted`) lives in that view: a finalizer runs inside
whichever call the collector interrupts, and through its own environment it
finds only its own plugin's values.

`run` and `files` are installed **per call**: `LuaHost::enter` puts the
plugin's own `run` and `files` into its environment, or takes them out. A plugin
that was not granted one does not get a function that refuses; it gets nothing,
which is the same world every other plugin has always had. Revoking is
therefore immediate. `layout.lua` runs in an environment of its own that never
holds either.

A capability once sat in the shared globals as `__run_impl`, and a leading `__`
is a naming convention rather than a boundary: an untrusted plugin could call
the other name and get a program run with the user's authority. Nothing a
plugin may not have is parked anywhere it can name. `tests/kernel_mvp.rs`
enumerates every global a plugin can reach and has no blanket exemption for a
leading underscore, and `tests/plugin_commands.rs` holds the hijack: a pane
that wraps `type` gets neither another pane's `run` nor its path on a command.

A capability asked for through `command` cannot be absent, since every plugin
has `command`. Those are gated where the command is applied instead: the queue
records the path each plugin's own `command` was built with, and
`Command::requires`/`Command::refusal` decide whether that plugin holds what
the command needs before the loop applies it — `run` for an exec automation's
command line or a URL opened unasked, `sessions` for every verb that drives an
agent or loses something for good. Every variant answers in one exhaustive
`match` with no catch-all arm, so a new verb does not compile until it decides. An
`action` runs the declaring plugin's handler, so every request carries the
chain of plugins it came through (`LuaHost::acting_for`, `api::requester`) and is
allowed only with what every plugin in that chain holds
(`Command::refusal_by`): neither the asker nor the handler can lend the other a
grant, through any number of hops.
`program` is checked the same way when its pane is started.

The grant itself comes from one place, `packages::granted` → `trust_of` →
`Trust::grants`, the same decision the Interface tab draws. A bundled file that
is byte for byte what the binary ships is `Shipped` and holds what it declares
without a trust record — the confirmation and the creation flow need `sessions`
before anyone could press `t` — and an edited one is decided like any other file.

The cost, accepted: a plugin must handle the absence, exactly as it handles a
session having no worktree. `examples/lua/composite.lua` shows the shape — draw
what is missing, not a blank pane.

## Events are derived, never raised

A plugin can declare `events = { … }` and an `on_event(name, payload)`, and the
loop calls it once per change (`kernel::events`, `coordinator::events`). The
kernel's own events come from **diffing published state** — the snapshot's rows
between two versions, the focus ring between two iterations, the command bus's
completions — never from the code that mutates it. Raising them from the code
that mutates would miss every other process (`thurbox-cli`, the cron tick, a
second interface, Fleet itself), and is the model where one mutation site
forgets to fire. The four `session.post_*` names are the only ones raised rather
than derived — from a *tracked* command finishing — which is why they carry the
operation's facts and fire only for this interface's own work.

One dispatch point per iteration, after the stores and the bus have published
and before the paint, so a handler's writes land in the frame about to be
painted. It is a `VecDeque::is_empty` on every quiet iteration and never marks
the frame dirty itself: a handler's `state` write bumps the state version and
its commands go through `dispatch_tracked`, both of which already do
(`frame-cost`). Emits from handlers are applied inside the same dispatch, which
is what makes the cascade bound (`MAX_DEPTH`) a bound rather than a
per-iteration trickle. A reload clears the queue, resets the deriver so the next
snapshot seeds silently, and puts `interface.reloaded` first.

The set is closed (`KERNEL_EVENTS`) with three readers — the loader, help and
`thurbox-cli plugin events` — so a name a plugin may subscribe to is always one
the kernel emits.

## Drawn is not the same as focusable

A `switch` slot draws one occupant, so an alternate is not on screen — but
focusing it is *what brings it forward*. Ask "is it drawn?" when deciding whether
focus may rest there and the answer is no, every time, because the slot records
its selection during render and the check runs before it. That is not a near
miss: it made the plugins pane impossible to reach at all, since the focus ring
skipped alternates by the same rule and `F11` was undone on the next frame. (That
pane is now a settings tab — see below — but the rule outlives it: the centre is
still a switch slot, so the next pane to share it inherits the fix.)

`kernel::focus` keeps the two questions apart — `is_drawn` for what the interface
reports about itself, `can_focus` for where focus may go. A pane that brings
itself forward by being focused needs the second one.

There is a third question, and it is not `can_focus` either: *should `Ctrl+H`/
`Ctrl+L` stop here?* A cycle that stopped wherever focus may rest stepped onto
every alternate, and stepping onto one draws it — walking the columns swapped the
agent's terminal for whatever else shared the centre. `next_in_cycle` makes a
switch slot one stop, its default occupant, and steps an alternate off the slot;
alternates are opened by their own key or pill, which `can_focus` still lets in.

An open float may consume that cycle with `on_focus_cycle("next"|"previous")`.
The coordinator offers it only to the topmost drawn float, and the host calls it
under the key hook's budget and environment. `true` leaves pane focus intact;
`false`, a missing hook or an error continues through `next_in_cycle`. Item order
and wrapping belong to Lua, so the kernel never learns what a card or row is.
Quit, reload and recovery keys retain their reserved handling.

There is a third fact underneath both, and it is a matter of *timing*: a pane can
open its own slot. The search strip shows itself and asks for focus in one action,
and `panels.show` is only read by the arrangement — so at the moment the request
is judged, the slot it named does not exist yet and focus is refused for a rect
that is one frame away. `defer_until_placed` is the answer: a request focus cannot
take is held for exactly one layout and re-asked there, which is why `ctrl+/`
leaves you typing in the strip rather than in the pane underneath it. Exactly one
layout, because a slot still unplaced then is one nothing brings forward.

## Settings are two halves of one screen

`settings.toml` and a plugin's declared settings are edited from the same
modal, and they behave differently on purpose. A plugin's value is in-process,
so it is written the moment the key is pressed. A core value is a *file* — and
some of them (mouse capture, the notifier thread, the scrollback a parser was
built with) were consumed at startup and cannot be un-consumed — so the core
half is a **draft**: `Ctrl+S` writes it, `Esc` discards it, and `⟳` marks the
rows that will not take effect until the next launch. `kernel::config` owns that
split; `session::settings::FIELDS` is its one definition, a row per field saying
`Live` or `NextLaunch`, and both `Config::adopt` and
`Settings::restart_only_differs` are derived from it so the two cannot drift.

That split has a second edge, and missing it cost the panel its edits: what the
modal shows and drafts from is `Config::on_disk`, **not** `Config::in_force`. A
restart-only change lands in the file and is deliberately withheld from what is
running, so seeding an edit from what is running silently proposes undoing it —
the next save of any row reverted it.

There is no process-wide copy of the settings: `session` holds no state. The
live half lives in `kernel::config`; the restart-only half is handed, as loaded,
to whatever is built with it — the backends (history depth), the terminals
(scrollback, how long a hidden grid stays), the snapshot store (git cadence).
Loading first is load-bearing rather than tidy: a process-wide copy that `main`
never filled would ignore the whole file while every switch looked honoured,
because every default matches.

## System chrome is not a pane

Help, settings and the theme picker are drawn by the kernel (`kernel::modals`),
above the arrangement and above plugin floats. They were plugins in the centre
slot, and that was the wrong shape: they competed with the terminal for the
slot, they joined the focus ring, and nothing could contribute *into* them.

So a modal is not in the layout, not in `focusable()`, captures input while
open, and closes on `Esc`; one is open at a time, and opening one closes the
other. Modularity moved one level down — plugins contribute **data**
(`Registry::bindings` → help, `Registry::settings` → settings) and the kernel
renders it, so declaring one table field is enough to appear in either. The
theme picker takes no contribution at all: there is nothing a plugin could add
to a list of palettes. The **command palette** (`Ctrl+P`,
`kernel::modals::palette`) is the fourth, and contributes in the other
direction: it lists the registry — keys, chord-less `commands` declarations, the
kernel's own — and `Enter` hands the loop a `Dispatch` that runs through the
same `on_action` a key press takes, after the modal has closed itself.

The **file list** — settings' Interface tab (`kernel::modals::interface`) — is here
for a second reason on top of those three. It is the recovery tool: it is where a
file that failed to load is reported with its error, where a bundled file is
restored (`r`) and where a plugin of the user's own is switched off (`space`) to get
a working interface back. A recovery tool that is itself a plugin can be the thing
that is broken, so a bad edit would take away the view you would use to undo it.
Being chrome also fixed its reachability: as a centre-slot pane it needed a chord of
its own, and that chord was `F11` — the one F-key terminals commonly claim for
fullscreen. Note the asymmetry the modal has to make legible, since the kernel is
the only thing that knows it: `r` writes back a copy the *binary* holds, so it has
no answer for a file the user wrote (`space` is that answer) or one a package
delivered (`thurbox-cli plugin sync` is).

The one genuinely modal input in the product lives here too: while help is
capturing a chord, **every** key is data, `ctrl+q` included. A plugin could
never be allowed to swallow the quit chord; the kernel can, because it knows
the capture lasts one keystroke.

## Two things that will bite you

**The render path must not read the plugin directory.** inotify reports a *read*
as an event, so a per-frame reader makes the host look like it is editing its own
plugins — which pushes the reload debounce forward forever, and auto-reload
silently never fires while `F10` works perfectly. `layout.lua` is loaded once per
reload for exactly this reason.

**A click is routed by identity, not by geometry a plugin declares.** The paint
walk records a hitbox for every node carrying `id`/`class`/`role`, parents before
children, and the loop scans that list *backwards* — so the innermost node under
a point wins, and a pane's own rect wins only when nothing inside it matched.
Change the recording order and the collapse chevron on a pane's border silently
starts focusing the pane instead.

**A pane cannot emit an escape, so a link has to be re-printed for it.** A
plugin returns cells; the kernel paints them. Nothing in that path can put an
OSC 8 sequence on the wire, which is why a url a pane draws was not
`Ctrl+Click`-able even though the identical text in an agent's transcript was —
the transcript's runs are re-printed wrapped in the escape after the frame
flushes (`paint_outer_hyperlinks`), and only sessions were offered to that
leg. The `url:<link>` click verb is that asymmetry closed: the verb's nodes ride
the same re-print, so the outer terminal owns the chord over them, and the
kernel resolves the chord against them too for an emulator that does not
understand OSC 8. The cells are read back out of the frame just drawn rather
than off the tree, so wrapping, alignment and scroll need not be re-derived —
and the covering surfaces are checked explicitly, because a pane's node has no
label to match against the buffer the way a vt100 run does.

**Terminal encodings differ, and folding them belongs in the kernel.** A legacy
terminal sends a bare `J` with no SHIFT; a kitty-protocol one sends `j` plus
SHIFT. `ctrl+/` arrives as one of three things. `registry::canonical_chord`
folds them, so a plugin declares one chord and it works everywhere. Both cases
were found by running it, not by reading the spec.

## Where the boundary sits

What `kernel` may reference is declared in `tests/architecture_rules.rs`, which
fails on any other edge.

## Testing

- `tests/kernel_mvp.rs` — the kernel against the *real* bundled plugins, so it
  fails if either breaks.
- `tests/kernel_limits.rs` — the instruction and memory bounds, in their own
  file because they mutate process-wide limits.

## What a frame costs

Every visible pane is rebuilt in Lua and converted back into nodes on each
paint, and **cost tracks node count, not data**: a synthetic plugin emitting 30
trivial rows costs the same as the real session list, and the Rust-side
conversion runs at roughly 9 us per node. Micro-optimising that conversion does
not pay; the levers that do produce fewer nodes or run the conversion less
often:

- **Paint less.** `publish` runs only before a paint or before dispatching input
  — the two moments Lua actually runs — and paints are capped at 60fps
  (`MIN_FRAME_INTERVAL`). A pane showing a terminal surface is gated on the
  pane's own output stamp (`Terminals::output_stamp`), so a quiet agent settles
  at the redraw floor.
- **Emit fewer nodes.** The session list spends 4 nodes per row (a box plus a
  border node either side). Drawing the borders as two full-height *columns*, as
  `20_agent.lua`'s `chrome()` already does, would take a 30-row list from ~90
  nodes to ~32.
- **Stop probing closed modals.** Every floating plugin is rendered full-screen
  each frame purely to discover it is not floating, unless the float declares
  `pure = true` (ADR-P16).

Dependencies are built with `opt-level = 3` even in dev builds
(`[profile.dev.package."*"]`), because `cargo run` is the documented way to run
thurbox from a checkout and an unoptimised Lua VM is felt in the UI.
