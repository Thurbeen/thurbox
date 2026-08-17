## Context

See `proposal.md` — Why, for motivation. The constraints that shape the
approach:

**The cut is already clean.** `tests/architecture_rules.rs` enforces that
`agent`/`git`/`storage`/`session`/`session_ops`/`cli` never reference `ui` or
`app`. An audit finds six references and all six are doc comments. The engine is
a library that does not know a TUI exists.

**A prior attempt exists and reported back.** Branch `thurbox-v2-with-openspec`
(PR #924) ported 5 of 6 panes to plugins with 2755 tests passing, then sent a
post-mortem. Its implementation is not being kept and nothing is being harvested
from it. Its findings are treated as evidence, and the decisions below record
which parts they invalidate and which they validate.

**A POC exists.** `~/poc/reloadable-ui-demo` — 1,340 lines of Rust and 479 of
Lua — demonstrates whole-VM reload, per-plugin error isolation, declarative
trees, persistent state across reloads, and slot composition. It is the starting
shape, with two structural corrections (D2, D1) applied before anything is built
on it.

**The runtime is Lua 5.4**, chosen for familiarity over Luau. This is a settled
decision; D8 records what it obliges the kernel to build.

## Goals / Non-Goals

**Goals:**

- A kernel with no pane in it — every surface, including the session list and
  the central agent view, is a replaceable Lua plugin.
- A node vocabulary that does not grow when a new appearance is needed.
- Enough geometry in a plugin's hands to express thurbox's real panes.
- Reads and commands that cannot block the render loop, on any host.
- A capability surface that stays at zero filesystem, process and network.

**Non-Goals:**

- Deciding how cross-plugin decoration resolves (D6). The first consumer does
  not exist yet; only the primitive that keeps both options open is in scope.
- Porting the info panel, file viewer, tasks, automations, code review, global
  search or the theme picker. Later changes.
- A stable, versioned plugin API. Public-but-unstable is the declared state.
- Plugin distribution, installation or update. The bundled set only.
- Mouse input beyond what node identity makes available for free.

## Decisions

### D1 — Four node primitives, with widgets in userland

The kernel's vocabulary is `text`, `box`, `input`, `surface`. Lists, gauges,
dividers, panels and tables are a Lua library.

*Why.* The prior attempt froze a catalog at 6 kinds and watched it reach 16
without becoming able to express code review. The named root cause is that its
userland widget library was specified and never built, so every appearance had
nowhere to live but a kernel enum variant — at a cost of enum arm, converter
arm, renderer arm, type definitions, regenerated goldens and a release, each.
That is v1's "edit ten parallel tables" tax, shrunk but not removed.

Most of what accumulated was not primitive: a gauge is a filled box, a divider a
repeated character, centring is padding arithmetic, a list is a column.

*Alternative considered.* Ship the POC's seven kinds (`text`, `vstack`,
`hstack`, `block`, `list`, `gauge`, `spacer`). Rejected: `list` and `gauge` are
exactly the two that belong in userland, and starting at seven starts on the
growth curve the evidence describes.

*Consequence.* The widget library is a first-class deliverable, not a
convenience. If it is not built, D1 fails the same way the prior attempt did.
This is the single highest-risk item in the change.

### D2 — Layout resolves before render

Rects are computed first; each plugin is then invoked with its own resolved
width and height.

*Why.* The prior attempt names this its biggest gap: a plugin returning a tree
blind to its rect cannot do percentage widths, wrapping, side-by-side splitting,
truncation, or derive a scroll window with overflow markers. One change closes
four of five recorded blockers and needs no new node kinds.

*Why the POC does not already do this.* It appears to — `ctx` carries `width`
and `height` — but `render_tab` passes the *body's* dimensions, then arranges
the returned trees afterwards in `tabs/<tab>.lua`. Render-then-arrange makes the
rect unknowable during render.

*Alternative considered.* A two-phase measure/layout pass, as CSS and Flutter
use. Rejected as unnecessary here: the arrangement is already a pure function of
the available size, so it can run first and one pass suffices. If intrinsic
sizing is ever needed, a measure phase can be added without changing the plugin
contract.

*Consequence.* The POC's render-then-arrange order is inverted. The arrangement
is expressed in Lua and receives dimensions rather than rendered trees.

### D3 — `surface` as a second rendering model

A surface carries cells rather than a tree. The kernel paints it within its
resolved rect. It is fed either by a live session or by a plugin.

*Why.* Some panes are geometry-first and are not trees at all. `code_review.rs`
is 2,971 lines and windows its body by character count against a resolved width;
side-by-side, wrapping, horizontal scrolling and syntax colouring are all
positional. Forcing them through a tree is what drove the catalog's growth.

The mechanism already exists: v1 paints every agent pane with `vt100::Parser`
plus `tui_term::PseudoTerminal` in `src/ui/terminal_view.rs`. Surfaces point
that at a second source.

*This also removes a carve-out.* The agent terminal is not a special kernel pane
— it is the first consumer of a general primitive, and code review will be the
second. In-node concerns that need live cells (drag text selection, reflow on
resize) belong to the surface node, and are stated as such rather than as
exceptions.

*Note.* The web arrived at the same split — a declarative tree *and* a canvas,
divided on the structure-first/geometry-first boundary. Convergent, not
imported.

### D4 — Node identity in the kernel, matching in userland

Nodes may carry `id`, `class` and `role`. The kernel does not resolve selectors.

*Why.* Any mechanism for decorating a node another plugin rendered must first be
able to find it, so identity is a precondition of every option rather than one
of them. Identity is one optional field and it also yields event targeting,
which replaces v1's hand-built per-frame `click_targets` registry of
`RowHitbox`/`ButtonHit`.

A selector engine is not built, because it would be the largest instance of the
mistake the prior attempt diagnosed: a matching language, specificity rules and
a resolution pass in Rust, ahead of any consumer. That attempt also records that
capabilities built ahead of consumers went unused — `input`, `tasks-write` and
`automations-write` all shipped with none.

*Alternative considered.* Selectors now, as the findings recommend and as
Textual demonstrates in a terminal. Deferred rather than rejected — see D6.

### D5 — Snapshot reads, queued commands

Reads are served from an in-memory snapshot the kernel refreshes on its own
schedule. Writes are commands that return immediately and surface through a
later snapshot. A plugin cannot wait on anything.

*Why.* Plugins render on the UI thread. Any read that could touch SQLite, git,
a subprocess or an unreachable SSH host would stall the loop. A single rule —
Lua never blocks and never awaits — removes the entire class, and removes it for
plugins nobody has written yet.

This also subsumes work v1 did case by case: ADR-P12 moved the whole new-session
flow off the UI thread, ADR-P6 cached hook state behind a `data_version` check,
ADR-P8 moved review diffs to a worker. Under D5 those are not optimisations but
the only available shape.

*Alternative considered.* Coroutine-based async, letting a plugin await a
command. Rejected: it puts scheduling and cancellation into the plugin contract,
and `PendingSpawn` demonstrates that in-flight work needs to be *rendered*
anyway — which a snapshot already provides.

*Consequence.* In-flight commands must be readable, or a plugin cannot draw the
placeholder row that v1's `PendingSpawn` draws.

### D6 — Cross-plugin decoration is deferred, not decided

Global search restyles rows in three panes it does not own. The prior attempt
recorded this in `tests/global_search_pane_gap.rs` as "a mode, not a pane" —
structurally unportable under a model where style lives on the node.

Global search is not in the bare core, so its first consumer does not exist. The
change ships identity (D4) and keeps two paths open: a userland tree-walk in
which a decorator transforms a slot's rendered tree, or a kernel selector engine
promoted from a userland `select` library once profiling justifies it.

*Why defer.* Deciding now means designing a mechanism against an imagined
consumer, which is how the node catalog grew. Deciding later means deciding with
a working system to measure against. The direction of promotion — userland to
kernel — is available; the reverse is not.

### D7 — The registry holds contracts, plugins hold surfaces

The kernel collects key and setting declarations, detects conflicts, applies
persisted overrides and routes. It renders nothing. The help and settings
screens are plugins that read the registry.

*Why.* Coherence has to be a property of the API, not a request in the docs.
Declarations as data mean a new plugin's keys appear in help and become
rebindable with no other file edited. Routing and conflict detection are
cross-plugin contracts that no single plugin can own — but presentation is not,
so it stays in userland.

*This replaces, not preserves, v1's machinery.* The `Action` enum, `KeyContext`,
`keybindings.json` and the F1 editor are deleted; the registry and its plugin
screens re-provide the behaviour.

### D8 — Lua 5.4, with kernel-supplied safety

The runtime is Lua 5.4 with a deliberately chosen set of opened standard
libraries.

*Why this needs recording.* Luau supplies `sandbox()`, `set_interrupt()` and
`set_memory_limit()`, and the prior attempt used all three successfully. Under
Lua 5.4 none is free: an unterminated loop in a plugin would hang the render
loop with no recovery.

*Therefore the kernel builds them.* An instruction-count hook bounds execution
and aborts the invocation as a plugin failure; an allocator limit bounds memory.
Both are named tasks, not assumptions. The capability surface is set by which
standard libraries are opened at all — enforcement by absence, per D9.

*Trade accepted.* Familiarity for third-party authors, at the cost of kernel
work the alternative would have donated.

### D9 — Capabilities enforced by absence

An ungranted capability is not present in the environment, rather than present
and refusing.

*Why.* This is the part of the prior attempt that was validated rather than
questioned: five panes ported with its manifest untouched and zero new grants.
The sharpest case is the file viewer, which needs file contents and launches
`$EDITOR` — both stayed kernel-side and the plugin only drew. Absence is a
stronger guarantee than CSP or iframe sandboxing, and it is explicitly flagged
as something not to regress.

*Consequence.* Every capability is introduced with its consumer, never ahead of
one.

### D10 — Plugins cannot reach the render thread's state

The Lua VM is not `Send`; mlua's `send` feature stays disabled.

*Why.* Also validated by the prior attempt: it makes "plugins never touch the
render thread" a compile error rather than a review rule. Free, and load-bearing
once third parties write plugins.

### D11 — Bundled plugins embedded, materialized, and the recovery floor

Bundled plugins compile into the binary, are written to the user's plugin
directory on first run, are shadowed by a user copy where one exists, and are
the fallback when the user's environment fails to load.

*Why.* With no pane in the kernel, a broken plugin directory means no interface
at all — a failure mode v1 could not have. The embedded copies make a blank
screen unreachable while keeping the shipped interface readable and editable as
ordinary files, which matters when extensibility is the point. v1's built-in
hooks extension already establishes the pattern.

### D12 — Repainting stays change-driven

A frame is painted when a plugin's view differs from its last, when input
arrives, or when the forced-redraw floor elapses.

*Why.* v1's demand-driven loop takes idle from ~100 fps to ~4. The prior attempt
confirms tree diffing before marking dirty is what holds the floor. Comparing
trees is the plugin-model equivalent of v1's `needs_redraw`.

### D13 — Retiring v1 is a separate change, gated on parity

Deleting `src/app/` and `src/ui/` moves out of this change into `v2-retire-v1`,
which cannot run until every v1 surface has a plugin equivalent proven against
its recording.

*Why this changed.* The original gate was "the bare core matches its
recordings". That prevents deleting something *unproven*; it does not prevent
deleting something *unreplaced* — and the bare core is seven plugins, so the
gate as written permitted removing themes, tasks, code review, search and the
file viewer on the strength of a session list and a terminal.

*Cost.* Two rendering paths are maintained until the tail is done. That cost is
real but bounded, and it is paid in a repository where the two halves provably
do not conflict — no code below the cut references either.

*Consequence.* The bare core proving itself now licenses nothing but the next
change. Every deferred pane has a named change and `v2-retire-v1` lists them as
prerequisites, so "later" is checkable rather than aspirational.

### D14 — The theme system belongs in the kernel, the picker does not

The kernel resolves the active theme — built-in presets, user definitions from
`themes.toml`, the choice persisted in `metadata.active_theme` — and publishes
its roles. Choosing one is a plugin.

*Why it is not a deferred feature.* Every plugin already expresses colour by
naming a role (D-level requirement in the `view-tree` spec), so *something* must
resolve those roles before any pane can draw. Shipping one hardcoded palette
would leave that requirement unmet, break the coherence constraint that produced
D7, and make v2 look nothing like the thurbox the user configured. It is the
coherence layer, not a feature.

*Why the picker is still userland.* Resolution is a contract; presentation is
not. The same split as D7's registry: the kernel collects and resolves, plugins
render. The theme list is published, so a picker is an ordinary plugin.

*What this reuses.* `session::theme_config` is pure data and already allowed to
the kernel — 36 presets and a 29-role palette — so this is wiring, not
reimplementation.

## Risks / Trade-offs

**The widget library is not built, and the catalog grows anyway (D1).** This is
the exact failure already observed once. → It is a deliverable of this change,
not a follow-up, and the first pane ported must be built on it so the pressure
is felt immediately rather than after the kernel has hardened.

**The bare core is too bare to evaluate.** A kernel with no usable interface
cannot be judged, and the prior attempt spent three rounds without deleting
anything. → The bundled set is scoped to operate sessions end to end, and the
first vertical slice runs to completion before the remaining panes are specified.

**Golden recordings are lost by deleting `ui/` first.** They can only be taken
while the v1 renderers exist. → Capturing them is an ordering constraint on the
task list, ahead of any deletion. Nothing is harvested from the prior branch, so
these are captured fresh from v1.

**The oracle is vacuous.** v1 has 7 acceptance snapshots and none renders pane
content — all were captured with no active session. "Snapshots must not move" is
a proof that cannot fail. → New recordings must assert pane content with an
active session, with the formatter destructuring every field by name so a
dropped field is a compile error.

**Deletion is licensed by a test that cannot fail.** The prior attempt found its
differential oracles compared a plugin against a builder *inside the module the
deletion removes*, so the repair that compiles is dropping the assertion. → No
deletion is licensed by a differential comparison. Recordings are taken while
the native renderer exists and asserted at both edges.

**Runaway-plugin protection is new code on a hot path (D8).** An instruction
hook fires constantly. → Budget generously, measure the overhead against v1's
frame timings, and treat the bound as a setting rather than a constant.

**Performance regressions are invisible until late.** ADR-P6 through ADR-P12 all
lived in `app/`. → v1's perf counters and their acceptance tests are re-derived
against the new render path as part of the vertical slice, not afterwards.

**The parity tail outlasts patience, and v1 is deleted early to escape it.**
The tail is five changes long. → The gate is structural rather than a matter of
discipline: deletion is its own change listing each prerequisite by name (D13),
so shipping it early means visibly removing a prerequisite, not quietly deciding
the bare core is enough.

**Lua 5.4 makes third-party plugins arbitrary code execution.** Consistent with
v1, whose `extensions/` already runs user-installed shell — but it should be
stated rather than discovered. → Documented in the plugin-authoring guide, and
the opened standard-library set is chosen deliberately rather than by default.

## Migration Plan

1. **Capture the oracle first.** Record what each v1 pane draws, cell-exact,
   with an active session, while `src/ui/` still exists. Nothing else may start
   until this exists.
2. **Kernel skeleton.** Plugin host, four primitives, layout-before-render,
   identity, surface, resource limits. Validated by the POC's own plugins.
3. **Host API against the real engine.** Snapshot and command bus over the
   untouched `session`/`storage`/`agent` layers.
4. **One pane end to end.** The session list, built on the widget library,
   proven against its recording. This is the point at which D1, D2, D5 and the
   registry are tested by something real rather than by a demo.
5. **The rest of the bare core.** Terminal surface, arrangement, footer, help,
   settings, spawn flow, confirmation.
6. **Delete.** `src/app/`, `src/ui/`, the v1 loop — only once every recording
   passes against the plugin implementation.

**Rollback.** Until step 6 the v1 TUI is untouched and remains the shipping
binary; abandoning the change costs only the new code. After step 6 rollback is
a revert of the deletion commit, which is why that commit is kept separate and
last.

## Open Questions

- The staleness bound for snapshot reads, and whether it varies by kind (session
  status against a remote host is inherently slower than a local row). Settable
  during step 3 without changing the specs.
- The instruction budget and memory cap in D8. Requires measurement against real
  plugins; the specs require only that bounds exist and are enforced.
- Whether the arrangement needs a measure phase for intrinsic sizing. D2 notes
  it can be added without changing the plugin contract, so it can wait for a
  pane that needs it.

## Findings from the first slice

Recorded per task 8.8, after building the kernel and taking the session list
end to end. These are things the specs did not anticipate.

**The POC's watcher had a latent bug that only appears with a per-frame reader.**
The arrangement was initially re-read from `ui/layout.lua` on every frame. inotify
reports a plain *read* as an access event, so the host watching its own plugin
directory saw an "edit" ~20×/s, and each one pushed the reload debounce window
forward — meaning auto-reload never fired at all, while `F5` worked perfectly.
It presents as "the watcher is broken" and is actually "the reader is too eager".
Two fixes, both kept: access events are filtered as noise, and the arrangement is
loaded once per *reload* rather than once per frame (which also removes a syscall
from the render path). **Consequence for later work: nothing on the render path
may read from the plugin directory.** Worth stating in the spec if a second
reader ever appears.

**Withholding standard libraries is not enough to withhold capabilities.**
Constructing the VM without `io`/`os`/`debug` still leaves `dofile` and
`loadfile` reachable, because Lua's *base* library is not optional and carries
them — both read arbitrary paths. `print` is a separate problem: stdout belongs
to the TUI, so a stray `print` corrupts the screen rather than logging anything.
The capability surface is therefore stdlib selection **plus** an explicit scrub
of named globals (`host::WITHHELD_GLOBALS`). This was found by the D9 probe test,
which is the argument for writing that test before believing the model.

**`set_interrupt` is Luau-only, as D8 anticipated — but the replacement is not
equivalent.** Lua 5.4's count hook fires every *n* instructions and aborts by
raising an error, so the budget is measured in hook batches rather than
instructions, and the abort surfaces as an ordinary plugin error. That is
adequate (an unterminated `render` costs one red pane) but it means the budget
is approximate, and the hook cost is paid on every plugin call. Task 2.12's
measurement is therefore load-bearing, not a nicety.

**Declaring a plugin's size statically is what makes the layout pass possible.**
D2 says rects resolve before render; the circularity that blocks this is that a
slot's division depends on its occupants' sizes. Moving the size declaration out
of the render *output* and into the plugin's declaration *table* breaks it with
one pass and no measure phase. This deserves to be explicit in the
`ui-composition` spec, which currently implies it without stating it.

**`slot_mode = "switch"` is untested with more than one occupant.** The centre
slot declares it, but only one plugin currently contributes there, so switching
degenerates to "render the only occupant". Tasks 6.5–6.7 are genuinely unproven,
not merely unimplemented — the first real second contributor to the centre slot
is what will validate the design.

### Findings from wiring the live terminal

**A backend must be readied before a pane can be adopted.** `Session::adopt`
fails with "Control mode not started" unless `ensure_ready()` has opened the
backend's control-mode connection. v1 does this for every backend at startup;
the kernel does it **lazily, once per backend, and only when a session on it is
actually attached** — so a configured-but-offline SSH host costs nothing until
you select a session on it. That is a deliberate improvement, but it puts one
blocking call on the loop (not inside a plugin). For a remote host that could
stall a frame, which is a command-bus problem to solve properly in group 4.

**Testing v2 against a dev build has a two-part trap worth writing down.** A dev
build compiles in the `thurbox-dev` tmux socket, but `THURBOX_DATA_DIR` — which
thurbox injects into every session it spawns — points at the *real* data dir. Run
v2 from inside a thurbox session and it reads real session rows while looking for
their panes on the dev socket, so every attach fails with "can't find pane". The
symptom looks like broken adoption and is actually a split environment. Run it
with those variables cleared (and `TMUX_TMPDIR` unset) so the database and the
socket agree.

**The pane must explain a failed attach.** Both of the above presented
identically — an empty box — until the attach error was published into the
snapshot and rendered. `attach_error` on a session row is now part of the read
API, and the agent pane shows it. This is the third time in this change that a
diagnosis was cheap only because the failure was surfaced rather than logged.

### Findings from the command bus

**`select` is not a command.** The spec lists it alongside delete and restart,
but which row the cursor is on is the session list's own state — it survives
reload in `state`, needs no database write, and no other plugin has an opinion
about it. Removing it from the command surface is a simplification the spec
should absorb, not a gap.

**Ordering is the one command with a read-modify-write shape.** Every other
command touches a single row; reorder reads all of them, swaps two and renumbers.
With a thread per command, holding a key down had two moves read the same order
and land as one. Fixed with a lock around ordering specifically, rather than
serialising the whole bus — which would have reintroduced the head-of-line
blocking the per-command threads exist to avoid.

**A reorder must respect the grouping the list draws.** Moving on the flat list
let a session swap past a repo boundary, which reordered the underlying rows
while the screen appeared not to change. The command now finds its neighbour
*within the same repo* and no-ops at a group edge. The repo label is shared with
the renderer (`snapshot::repo_name`) so the two cannot disagree — the same class
of bug as publishing a field the plugin then sorts by.

**Publishing is a second place a field can go missing.** `display_order` was in
the snapshot and in the database, and reorder commands were writing it
correctly — but `publish` never emitted it, so the list sorted by a value that
was always nil and every reorder looked like a no-op. The snapshot struct and
the published table are two separate lists of fields, and nothing checks they
agree. Worth a test that asserts every `SessionRow` field reaches Lua before
this change is archived.

**Two input findings worth carrying into the registry work (group 5).** A legacy
terminal sends a bare `J` byte with no SHIFT modifier, and the host lowercases
`key.key` — so matching capitals on `key.shift` silently falls through to the
plain-letter branch. `key.char` is the only place the distinction survives.
Whatever the registry accepts as a chord has to account for this, or every
capital binding will be subtly broken on terminals without the kitty protocol.

### Findings from the theme system

**Publishing the palette made the coherence rule checkable.** With colours
resolved by the kernel and named as roles, "no plugin hardcodes a colour"
stopped being a convention and became a test that greps the bundled plugins for
literals. That test found nothing — but it will, the first time someone reaches
for `"#5fafff"` because a role was missing.

**The theme picker is the first genuine test of `switch` mode.** Until it
existed, the centre slot had one occupant and switching degenerated to "render
the only thing there". The rule that emerged is simpler than the spec
anticipated: *focusing a plugin in a switch slot makes it the visible one*. That
drives switching and satisfies "focus never rests on a hidden pane" with one
mechanism instead of two, so tasks 6.5–6.8 close together.

**Lua's `#` is a byte count, and it shows.** A theme named "Rosé Pine Moon"
padded one column short in the picker. Fixed by opening `StdLib::UTF8` — pure
computation, no capability risk — and measuring with `utf8.len`. **This is still
not display width**: a CJK or emoji character occupies two columns and counts as
one. v1 links `unicode-width` for exactly this, so true width belongs in the
kernel as a published helper, and the prior attempt's findings record reaching
the same conclusion ("let a plugin trim a string the way thurbox trims one").
Worth doing before any pane renders user-supplied text.

**`theme` is the one command that names no session and skips the bus.** It
mutates in-process state a worker thread cannot reach, and it is instant, so
nothing is gained by making it asynchronous. It is applied by the loop before
dispatch. Kept in the `Command` enum anyway so adding a command stays a compile
error in `execute`, rather than a silent no-op.

### Findings from the registry and floating plugins

**The chord canonicaliser is where the terminal-encoding trap gets fixed once.**
A legacy terminal sends a bare `J` with no SHIFT; a kitty-protocol one sends `j`
plus SHIFT. `canonical_chord` maps both to `shift+j`, so a plugin matches on an
*action* and never sees the difference. That is a better answer than the session
list's original `key.char == "J"` workaround, which would have had to be
repeated in every pane.

One asymmetry worth knowing: a capital written *bare* means shift (`"J"` is
`shift+j`), but a capital written *with a modifier* does not (`"Ctrl+D"` is
`ctrl+d`, not `ctrl+shift+d`). Reading it the other way would silently move
every hand-written binding, so the rule only fires when nothing else was
written.

**Floating is a property of the frame, not of the plugin.** A modal declares
`floats = true` statically — so the kernel knows to render it after the
arrangement — but *is* open only on the frames it returns a `float` node. There
is no open/close state for the kernel and the plugin to disagree about, which is
the failure mode a `Modal::None` variant invites.

**A modal needs no knowledge of what it is confirming.** The asker publishes the
question *and the command to run* through `store`; the confirm plugin renders
the text and issues whatever it was handed. So the session list can ask about a
worktree without the modal knowing sessions exist, and the next pane that needs
a confirmation writes no new modal.

**Help and settings prove the registry earns its keep.** Both render
`thurbox.registry` rather than a list they maintain, so a plugin added tomorrow
appears in help with no edit to help. The bundled set already declares 16 keys
across four panes with zero conflicts — several panes declare `j`, which is fine
because each is plugin-scoped and focus decides.
