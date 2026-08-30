---
name: thurbox-performance
description: The thurbox render loop's performance contract: demand-driven redraw and the two frame floors, what marks the screen dirty, reflow full-repaints, the republish change-gates and pure-pane memos, the age-carrying cache rule (a cache here needs a TTL/generation), the vt100 two-row floor, and the perf HUD/histograms. Use when touching the render loop, republish, snapshot caches, adding a cache, or investigating thurbox CPU/frame cost.
---

# Thurbox render-loop performance

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Performance (render loop)

The loop is **demand-driven**: it paints when something changed or when the 250 ms
forced-redraw floor (`FORCE_REDRAW_INTERVAL`) elapses, never on every iteration.
There are **two floors between paints**, because typing has to feel instant and
watching a log scroll does not: `MIN_FRAME_INTERVAL` (16 ms) when a person did
something — a key, a resize, a worker result they asked for, tracked by
`input_dirty`, which is set with `dirty` through the one `App::note_input` and
never alone — and `OUTPUT_FRAME_INTERVAL` (33 ms) when the only thing owing a
frame is agent output. Applying the tight floor to both made a chatty agent drive
60 paints a second to show 30 lines; the split is worth 30% of the loaded cost
(ADR-P17). What marks the screen dirty:
any input, a resize, a reload, a worker result, and **new agent output** —
`Terminals::output_generation` is summed each iteration, which is what stops a
printing agent being drawn at 4 fps. A **reflow** — the arrangement placing a
slot at a new rect, so a column opened or closed — additionally forces one *full*
repaint: the cell diff is only correct while ratatui and the terminal agree on a
glyph's width, and where they cannot (a flag, an emoji presentation sequence) the
pane that closed leaves characters behind.
`kernel::paint::normalize_ambiguous_width` strips the one such disagreement that
is strippable — `U+FE0F` — from every painted cell, panes and vt100 surfaces
alike, and `kernel::paint::force_full_repaint` covers the rest by marking every
cell of the reflowed frame as one the diff must print. It is deliberately **not**
`Terminal::clear`: erasing flushes a blank screen and leaves the repaint to the
next flush, so every pane toggle blinked the whole interface.

A frame is more expensive than v1's, structurally: every pane is a Lua call
returning a table that is converted to nodes and painted. The **conversion** is
the surprise in that sentence and the biggest single cost in a frame — bigger
than running the plugins' Lua. `kernel::convert` therefore reads each node's
fields in **one `pairs` pass** rather than ~25 keyed lookups, and builds its
error paths as a borrowed chain (`Crumb`) rather than a `String` per node; both
are measured in `docs/PERFORMANCE.md`. So the loop settles
aggressively. `draw` compares each plugin's returned tree against the last one and
only marks the frame changed when it differs; a float does the same against its own
last tree and rect, and a **chrome band** compares the *cells* it just painted
against the ones it painted last frame — it has no tree to diff, and marking it
changed for having been *drawn* held `dirty` set after every frame, which stopped
the loop settling at all: an idle interface with no sessions repainted at the frame
cap forever (~32% of a core, against ~6% once it settles). Neither an open float
nor a live text selection marks the frame changed by itself — both used to,
which pinned the loop at the frame cap for as long as the creation wizard was
open. The perf HUD is the deliberate exception: its
counters move every iteration, so it says so.

Everything that touches the world runs on a worker and publishes back (rule 5):
`kernel::terminal` (attach — the sharpest teeth, since a down host runs out its ssh
timeout and adopting a pane needs the runtime *entered* on the worker),
`kernel::command`, `kernel::diff`, `kernel::metrics` (three cadences, one published
result — the clearest one to copy), `kernel::repos` (the only *parameterised* reads,
asked for by leaving a key in `store`), `kernel::runs`, `kernel::updates`.

Cached answers carry an **age**, not just a value. The mistake this repeatedly
invited was storing "we have an answer" where "the answer is current" was needed:
git stats froze at their first reading, a `run` refresh started a process per frame,
a failed branch fetch stuck for the process lifetime, a backend surveyed once
was treated as surveyed since, and a session's diff held its first computation
for the life of the process. Each is now a TTL, an in-flight marker, or a
generation counter — if you add a cache here, give it one (ADR-P13/P18; the one
exemption, the repo-name cache, keys on an origin URL that cannot move within a
process). The same rule holds one level up for anything issued **on a timer
against a host**: window discovery is throttled per backend, at 500 ms locally
but `REMOTE_DISCOVERY_INTERVAL` over ssh, and a survey or a mirror pass that
failed backs off further still (ADR-P19). Sharing made remote rows discoverable
and the one shared clock behind that throttle then cost two ssh commands a
second for as long as a single remote row stayed unattached.

`republish` — the one call that rebuilds every `thurbox.*` table — runs once per
painted frame and **once per input batch**, not once per event: a held-down key
otherwise paid for it per repeat. Within it **every** group is **gated on a
change-signal** (`SnapshotStore::version`, `Themes`/`Registry::version`,
`Terminals::meta_version`/`failed_version`, and the loop's `data_epoch` — which
moves on every worker result and command transition and deliberately never on
agent output, so a streaming turn reuses `diffs`, `links`, `content`,
`commands` and `metrics` whole; the parameterised reads pair the epoch with a
digest of the question, which is also what gives their tables the stable
identity the panes' own `rawequal` memos key on). A group whose inputs did not
move is not rebuilt; and a pane that declares
`pure = true` has the tree it last returned reused — a cache hit is a refcount
bump on an `Rc` tree, and the settle diff short-circuits on pointer identity.
This is ADR-P16 closed out by ADR-P18, and it all rests on one rule: a signal is bumped **inside** the
mutation and only when the value actually changed — writing an unchanged value
counts as no change, which is the difference between the gate saving 27% and
saving nothing. The **animation clock** obeys it too: it lives in the epoch and
the loop advances it only while something is actually animating, because a
free-running one invalidated every pure pane on every idle frame. It is also
**scoped to its readers**: `ctx.elapsed` is served through the render context's
metatable rather than set as a field, so the kernel can see which panes asked for
it, and a pure tree is keyed on the animation tick only if the render that built
it read the clock (`CachedTree`). Every other TUI gets that coupling for free
because the animating widget is the one that asks to be redrawn — a Textual
widget's `set_interval(…, self.refresh)`, a Bubble Tea spinner's own tick command,
fidget.nvim's `Anime` closure — and thurbox's panes do not ask, so it is observed
instead. Detected rather than declared on purpose: a declaration defaulting to
"does not animate" freezes a third-party spinner silently (ADR-P21, +51% down to
+12% under load). And the loop
itself slows its input poll to `IDLE_TICK` once nothing has happened for
`QUIESCENT_AFTER` — free, because `event::poll` returns the instant an event
arrives, so only things that never wake the thread are delayed.

The reads in `republish` that touch a screen or the
disk carry the age above (ADR-P14): link extraction is keyed on that session's
`output_stamp` **and limited to surfaces actually on screen** — and the row
extraction itself is computed once per output stamp and shared by the link
scan, the click-time URL resolve and the OSC 8 repaint (a link nothing
painted can be neither clicked nor handed to the outer terminal, and the scan
walks a whole vt100 grid — doing it for every live pane cost ~1.2ms a frame with
three of them), the search content scan on `output_generation`, and the interface
inventory's per-file digests on a `trust_stale` flag every path that changes the
directory or a grant already sets. The link scan carries a **second** gate,
`LINK_SCAN_INTERVAL` (250 ms), because the stamp is exact for a screen that has
stopped and no gate at all for one that has not: a printing agent moves it every
frame, and a scrolling screen puts its URLs on new rows each time, so the scan
found a real change per frame and moved the data epoch — which un-gated every
group and every pure pane for anyone whose agent prints a URL, i.e. all of them
(ADR-P20: printing a URL cost +66% CPU under load, now +15%). A per-frame recompute whose
answer legitimately changes is the way a change-signal moves that nobody is
looking for; compare-before-store asks whether the value moved, and the missing
question is whether it was worth asking yet.

**A vt100 grid is never given fewer than two rows or two columns**
(`agent::backend::vt_floor`). A cramped layout really does compute a one-cell pane,
and vt100 underflows on the next byte written into one — in `row_inc_scroll` when a
line wraps, in `col_wrap` (`cols - width`) when a double-width character arrives.
The panic lands on the session's *reader* thread, so the process lives while that
session's terminal is blank for the rest of the run: the unwind poisons the parser
mutex, and every reader of it (paint, links, selection, copy) reads a poisoned lock
as "no live terminal". A panic is also written to `thurbox.log`, because a worker's
stderr is scrolled away long before anyone looks.

**Measuring it**: two instruments, both outside the PR gate (ADR-P5).
`cargo bench --bench frame_cost` times the *pieces* of a frame against the real
`ui/` — publish, arrangement, each placed pane, the paint, the vt100 surface and
link scan — modelling what `draw` does rather than what the plugin list contains
(a closed search strip occupies no slot, so nothing renders it).
`scripts/dev/perf-run.sh` runs the *whole binary* under a reproducible load in an
isolated sandbox and reports CPU from `/proc` beside the loop's own
`perf_window` line. A reading is only comparable with another at the same
terminal size and session count, so both pin theirs, and a change is argued with
a paired before/after rather than two absolute numbers.

**Observability**: `F12` toggles the perf HUD (`[features] perf_hud`); launching with
`THURBOX_PERF_LOG=1` writes `startup`, `perf_window` and `slow op` lines to
`thurbox.log`; while either is active a JSON snapshot is published for
`thurbox-cli perf`. Three histograms, kept separate so they **decompose** rather
than nest: `frame` is the paint, `republish` is the per-frame table rebuild
above, and `tick` is the rest of one iteration. `kernel::perf::snapshot_json`
owns the published shape and `cli::perf` only renders it. Full rationale:
`docs/PERFORMANCE.md`.

