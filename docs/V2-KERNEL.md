# The v2 plugin kernel

thurbox v2 is a session engine with a Lua-driven renderer. The kernel owns no
pane: the session list, the terminal and every other surface that shows *your
work* is a plugin under `ui/`. The bundled set is currently **three** — the
session list, the agent pane, and the pane that lists the interface's own files
— plus the new-session flow, which floats rather than filling a slot; the
interface was cut back to its core and v1's other surfaces are listed
with their gaps in `openspec/changes/v2-parity-gaps/`. It does own the **system modals** — help,
settings and the theme picker — which are chrome about thurbox itself rather
than panes (see below). Written for someone about to change the kernel; if you
want to *write* a plugin, read `docs/PLUGINS.md`.

Writing a plugin starts at `docs/PLUGINS.md` — **Start here**, which is four
`thurbox-cli plugin` commands and needs no terminal.

Runs as `thurbox`. It **is** the interface now: `src/app/` and `src/ui/` were
deleted when the kernel took the binary name, so there is no second interface to
fall back to inside the process. v1 is maintained on the `v1.x` branch and still
takes patch releases.

The name matters more than it looks. The updater in an already-installed binary
hard-fails on a known binary missing from a release archive and swallows the
error, so an archive that dropped the name `thurbox` would end auto-update for
every install already out there, silently and unfixably. Inheriting the name was
the only safe direction — see `docs/RELEASING.md`.

A profile with v1 history is asked once, before the interface takes the terminal
(`kernel::consent`): it names every surface that is gone, says the database is
shared and unmigrated, and declining turns auto-update off and prints how to
reinstall 1.x. What is still owed to v1 is listed in
`openspec/changes/v2-parity-gaps/`.

Two of those surfaces are answered by panes rather than by the kernel, which is
the mechanism working as designed:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) (v1's diff
reviewer, the first consumer of `thurbox.diffs`) and
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) (v1's info
panel, drawn entirely from the snapshot). Each is its own repository, installed by
clone — `docs/PLUGINS.md` has the commands and what the two demonstrate.

## Shape

```text
   KERNEL (src/kernel/)                  LUA (ui/)
   ─────────────────────────             ──────────────────────────
   node      four primitives             layout.lua    arrangement
   layout    rects before render         lib/theme     roles
   convert   table <-> node              lib/widgets   list, gauge, panel…
   paint     node -> ratatui             lib/tree      decoration helper
   host      VM, reload, isolation       plugins/*     2 panes
   registry  keys + settings
   modals    help, settings, theme
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
   metrics   machine, agent, account
   perf      counters
   bundled   embedded interface, and what was done to your copy of it
   inventory which files exist, and which are running
```

## The five rules

**1. Four node kinds, forever.** `text`, `box`, `input`, `surface`. Everything
else composes in `ui/lib/widgets.lua`. A prior attempt froze its catalog at six
and reached sixteen, because it never built the userland layer and each new
appearance had nowhere else to live. `tests/kernel_mvp.rs` asserts the count.

**2. Layout resolves before render.** Rects are computed first, then each plugin
is called with its own. A plugin that does not know its width cannot wrap,
truncate, or derive a scroll window — this was the single biggest gap the prior
attempt reported. The circularity is broken by plugins declaring their size
*statically*, in their declaration table, not in render output.

**3. Snapshot-read, command-write.** Reads are served from an in-memory snapshot
and return instantly. Writes are commands that are accepted and surface later.
Lua never blocks and never awaits, so no plugin — including ones nobody has
written — can stall the loop on SQLite, git, or an unreachable host.

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
clearest to copy: `sysinfo` walks `/proc`, a pane's pid is a control-mode round
trip, and account usage shells out to `curl` — three cadences, one published
result. `kernel::repos` is the newest, and adds the one wrinkle the others do not
have: its reads are *parameterised* — which directory, which repository — and a
plugin cannot call something that waits, so the flow asks by leaving a key in
`store` and the loop serves it.)

## A capability that cannot be absent and useful

"Capabilities are absent rather than blocked" is easy while the answer is always
absent: there is no `os`, no `io`, no `package`, and `thurbox.yml` says so. The
first capability a plugin can be *granted* breaks that symmetry — it has to be
present for the plugin that may use it and absent for every other, in one shared
Lua state.

So it is installed **per call**. `LuaHost::enter` stamps the current plugin and,
in the same breath, sets `run` to the implementation or to nil. A plugin that was
not granted it does not get a function that refuses; it gets no function, which
is the same world every other plugin has always had. Revoking is therefore
immediate: the next call builds the environment without it. `enter_nothing` is the
other half, for Lua that belongs to no plugin — `layout.lua` declares no
capabilities and has no trust record, so it must not inherit the last grant.

**Where the implementation itself lives is the whole of whether this works.** It
is held in the VM's *registry*, never in its globals, because a plugin chunk's
`_ENV` **is** the globals table: anything parked there is reachable by name from
every plugin, granted or not. It sat in globals as `__run_impl` once, and a
leading `__` is a naming convention rather than a boundary — `scrub_globals` can
only remove names it lists, so an untrusted plugin declaring no capabilities could
call the other name and get a program run with the user's authority. The registry
is not addressable from Lua at all and still dies with the VM, which is what the
globals placement was reaching for. `require`'s module cache moved for the same
reason: reachable means rewritable, and a rewritten `lib.theme` would be handed to
every plugin. `tests/kernel_mvp.rs` enumerates the plugin environment and has no
blanket exemption for a leading underscore — it used to, which is why none of this
was caught.

The cost, accepted: a plugin must handle the absence, exactly as it handles a
session having no worktree. `docs/examples/composite.lua` shows the shape — draw
what is missing, not a blank pane.

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
split; `Settings::restart_only_differs` is the shared definition of it, asserted
against in both places so the two cannot drift.

That split has a second edge, and missing it cost the panel its edits: what the
modal shows and drafts from is `Config::on_disk`, **not** `Config::in_force`. A
restart-only change lands in the file and is deliberately withheld from what is
running, so seeding an edit from what is running silently proposes undoing it —
the next save of any row reverted it.

`settings::global()` is still a write-once `OnceLock`, shared with `thurbox-cli`
and v1, and it stays that way: it is what makes those callers safe. The live half
lives in `kernel::config` instead. Publishing at startup is load-bearing rather
than tidy — `Database::open` prunes the audit log to `audit_retention_days`, so a
kernel that has not published its settings prunes to the default. (v2 shipped
that way for a while: the kernel's `main` never called `settings::init`, so the whole file
was silently ignored while every switch looked honoured, because every default
matched.)

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
to a list of palettes.

The one genuinely modal input in the product lives here too: while help is
capturing a chord, **every** key is data, `ctrl+q` included. A plugin could
never be allowed to swallow the quit chord; the kernel can, because it knows
the capture lasts one keystroke. See
`openspec/changes/v2-system-modals/design.md` (D1–D3).

## Two things that will bite you

**The render path must not read the plugin directory.** inotify reports a *read*
as an event, so a per-frame reader makes the host look like it is editing its own
plugins — which pushes the reload debounce forward forever, and auto-reload
silently never fires while `F5` works perfectly. `layout.lua` is loaded once per
reload for exactly this reason.

**A click is routed by identity, not by geometry a plugin declares.** The paint
walk records a hitbox for every node carrying `id`/`class`/`role`, parents before
children, and the loop scans that list *backwards* — so the innermost node under
a point wins, and a pane's own rect wins only when nothing inside it matched.
Change the recording order and the collapse chevron on a pane's border silently
starts focusing the pane instead. v1 spells the same rule the other way round
(specific targets recorded first, first match wins).

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

`kernel` may reference `session`, `storage`, `sync`, `paths`, `session_ops`,
`git`, `notifications` and `clipboard`, plus `agent` by fully-qualified path
only — enforced by `tests/architecture_rules.rs`. It may never reference `ui` or
`app`: it is their replacement, not their peer.

## Testing

- `tests/kernel_mvp.rs` — the kernel against the *real* bundled plugins, so it
  fails if either breaks.
- `tests/kernel_limits.rs` — the instruction and memory bounds, in their own
  file because they mutate process-wide limits.
- `tests/v1_recordings.rs` — *removed with v1*. It held golden recordings of what
  v1 drew, recorded to files rather than compared against a live v1 builder
  because a differential oracle cannot license a deletion. It served its purpose:
  the deletion happened, and the recordings had nothing left to be the contract
  for. `git log -- tests/v1_recordings.rs` is where they went.

## What a frame costs

v2's frame is far more expensive than v1's, and the reason is structural rather
than a hot spot: every visible pane is rebuilt in Lua and converted back into
nodes on each paint. Measured on an 8-session snapshot (`opt-level = 1`):

These figures were measured against the **fifteen-plugin** interface, and the
bundled set is now three — so the totals no longer apply, while the per-pane costs
and the shape of the finding do:

| | cost |
|---|---|
| `publish` (all readable state) | ~1.1 ms |
| session list | ~1.5 ms (~1.2 ms of it with **zero** sessions) |
| help (measured while it was still a plugin) | ~1.2 ms |
| agent pane | ~0.5 ms |
| footer (since removed) | ~0.2 ms |
| float probes (5 modals, all closed; all since removed) | ~1.3 ms |
| **visible set, 150x30** | **~5.2 ms** |
| **visible set, 250x60** | **~9.3 ms** |

The shape of that is worth internalising: **cost tracks NODE COUNT, not data.**
A synthetic plugin emitting 30 trivial rows costs the same ~1.2 ms as the real
session list, and pre-building its tree in Lua saves only 17% — the rest is the
Rust-side conversion, at roughly 9 us per node. Micro-optimising that conversion
does not work; removing the per-span `HashMap`, switching to `raw_get`, and
dropping the per-node path `format!` each moved it by under 5%.

So the levers that actually pay are the ones that produce fewer nodes or run the
conversion less often:

- **Paint less.** `publish` runs only before a paint or before dispatching input
  — the two moments Lua actually runs — and paints are capped at 60fps
  (`MIN_FRAME_INTERVAL`). Input is still polled every 10ms, matching v1.
  A **surface** used to defeat this outright: its cells live outside the tree, so
  a pane showing one was treated as changed every frame and the whole
  demand-driven scheme collapsed to a steady 60fps whenever a terminal was
  visible. It is now gated on the pane's own output stamp
  (`Terminals::output_stamp`), the same atomic v1 reads in
  `detect_output_redraw`, so a quiet agent settles at the redraw floor.
- **Read less.** The snapshot rebuild is gated on `PRAGMA data_version`, so an
  idle thurbox stops re-reading five tables (plus one query per automation)
  every 400ms. Git stats are folded in either way — they arrive from workers,
  about which the pragma says nothing. v1's ADR-P6, same mechanism.
- **Emit fewer nodes.** The session list spends 4 nodes per row (a box plus a
  border node either side). Drawing the borders as two full-height *columns*, as
  `20_agent.lua`'s `chrome()` already does, would take a 30-row list from ~90
  nodes to ~32.
- **Stop probing closed modals.** Every floating plugin is rendered full-screen
  each frame purely to discover it is not floating. No bundled plugin floats any
  more, so this costs nothing today — and is the trap to avoid when the modals
  come back.

Dependencies are built with `opt-level = 3` even in dev builds
(`[profile.dev.package."*"]`), because `cargo run` is the documented way to run
thurbox from a checkout and an unoptimised Lua VM is felt in the UI.

## The gap to v1

Two lists, and they answer different questions. `tests/v2_parity.rs` holds the
**rendering** divergences — what v2 draws differently — and asserts their exact
count, so adding one without recording it fails. `openspec/changes/v2-parity-gaps/`
holds the **behavioural** ones, found by auditing each v1 pane against its
plugin; it is the longer and more serious list, and Tier 0 of it is what
`v2-retire-v1` is really waiting on.

## Decisions

Each is recorded with its alternatives in
`openspec/changes/v2-plugin-kernel/design.md` (D1–D14), and each later change
carries its own. The "Findings from implementing" sections are the honest part:
what the design got wrong, discovered by building it.
