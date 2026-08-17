## Why

Thurbox's UI is 50,327 lines of Rust — `app/` (33,485) plus `ui/` (16,842) — of
which `app/mod.rs` alone is 14,705 lines carrying 616 methods on a single `App`
struct. Every pane is welded to that struct: adding one means editing a dozen
parallel tables (an `InputFocus` variant, a `Modal` variant, an `Action`, a
layout branch, a key-context arm, a snapshot test). The cost of a new surface is
paid in Rust, by recompiling, by the maintainer — never by the user.

That is the wrong shape for what thurbox is becoming. The interesting work is no
longer "which panes ship" but "which panes *you* want", and the product should
follow pi's model: a small kernel that does the irreducible thing, and an
extension surface where everything else lives. Today no such surface exists.

The enabling observation is that this is cheap. `tests/architecture_rules.rs`
has been enforcing `agent`/`git`/`storage`/`session`/`session_ops`/`cli` ← never
`ui`/`app` since the beginning. Auditing it confirms **six** references across
the lower half and **all six are doc comments**. The UI is not entangled with
the engine; it sits on top of it. v2 is not a rewrite — it is deleting the top
half and writing a new one against an API that does not move.

A prior in-place attempt (branch `thurbox-v2-with-openspec`, PR #924) ported 5
of 6 panes and sent back a post-mortem. Its implementation is not being kept;
its findings are, and they shape this proposal — both what to adopt and what to
avoid.

## What Changes

- A **second binary** (`thurbox2`) runs the plugin kernel alongside the v1 TUI.
  Nothing in `src/app/` or `src/ui/` is touched by this change: retiring v1 is
  its own change (`v2-retire-v1`), gated on **full feature parity**, not on the
  bare core alone.
- A **plugin host** is introduced: a Lua 5.4 VM, rebuilt wholesale on reload,
  with per-plugin error isolation and state that survives a reload.
- The kernel gains a **view tree of four primitives** — `text`, `box`, `input`,
  `surface`. Not a widget catalog. Lists, gauges, panels and dividers are a
  userland Lua library, so a new widget costs a file save rather than a release.
- A **constraint pass runs before render**: layout resolves rects first, then
  each plugin renders into its own resolved rect. Percentage widths, wrapping,
  truncation and scroll windows become expressible.
- Nodes carry **identity** (`id`/`class`/`role`), which serves both styling and
  event targeting — replacing v1's hand-built per-frame click-target registry.
- A **`surface` primitive** carries geometry-first content as cells, painted by
  the existing `vt100::Parser` + `tui_term` path. The agent terminal is a
  session-fed surface; code review will be a plugin-fed one.
- A **host API** exposes the session engine to Lua as snapshot reads and queued
  commands. Lua never blocks and never awaits.
- Plugins **declare** their keys and settings as data. The kernel collects them,
  detects conflicts, applies user overrides and routes; help and settings
  screens are themselves plugins rendering what the kernel collected.
- Slots gain **modes** — `stack` (children share space) and `switch` (one child
  visible, others are tabs) — plus **z-order and key grab**, which makes modals
  ordinary floating plugins.
- **BREAKING** — the session list, the central terminal, the footer, help and
  settings all become Lua plugins. No pane is kernel-owned.
- Bundled plugins are **embedded in the binary and materialized to disk** on
  first run, with the embedded copies as the fallback when a user's copy is
  broken or missing.
- The **whole v1 theme system carries over**: all 36 presets, user themes from
  `themes.toml`, and the active choice persisted in `metadata.active_theme`. The
  kernel resolves the active theme's 29 roles and publishes them; plugins name
  roles and never colours, so a theme change restyles every pane at once.
- Existing **`keybindings.json` overrides are migrated** into the registry rather
  than discarded, so a user's bindings survive the move.
- **BREAKING** — v1's `Action` enum, `KeyContext` scoping, `keybindings.json`
  and the F1 binding editor are removed; their behaviour is re-provided by the
  registry and its plugin-rendered screens.
- The bare core ships far fewer surfaces than v1. Info panel, file viewer, tasks
  panel, automations pane, code review, global search, session creation/fork/sync
  and the terminal affordances (mouse, clickable URLs, notifications) are not in
  this change. Each is planned as its own change — `v2-session-flows`,
  `v2-workflow-panes`, `v2-navigation-panes`, `v2-code-review`,
  `v2-terminal-affordances` — and **v1 is not retired until all of them land**.

Deliberately **not** decided here: whether cross-plugin decoration (global
search restyling rows in panes it does not own) resolves to a kernel selector
engine or a userland tree-walk. Global search is not in the bare core, so the
first consumer does not yet exist. This change ships only the enabling
primitive — node identity — which keeps both doors open.

## Capabilities

### New Capabilities

- `plugin-host`: VM lifecycle, whole-VM reload, per-plugin error isolation,
  private/shared state across reloads, and the resource limits (instruction
  interrupt, memory cap) that stop a runaway plugin from hanging the TUI.
- `view-tree`: the four node primitives, node identity, the constraint pass that
  resolves rects before render, and how a tree becomes painted cells.
- `plugin-host-api`: what a plugin may read (snapshots of sessions, repos, git)
  and command (spawn, delete, restart, fork, focus, send), plus the capability
  model — enforcement by absence, with no filesystem, process or network
  binding present in the plugin environment.
- `ui-composition`: slots and slot modes, focus routing and key dispatch order,
  z-order and exclusive key grab, and the responsive layout contract.
- `plugin-registry`: how plugins declare keys and settings, how conflicts are
  detected, how user overrides are persisted and applied.
- `bundled-plugins`: how bundled plugins are embedded, materialized, shadowed by
  user copies and fallen back to — and which plugins constitute the bare core.
- `theming`: how the active theme is resolved from presets, user definitions and
  the persisted choice, how its roles reach plugins, and what changing it does.

### Modified Capabilities

None. `openspec/specs/` is empty; this change establishes the first specs.

## Impact

**Deleted by this change: nothing.** v1 keeps shipping untouched. The ~50,300
lines of `src/app/` + `src/ui/` are removed by `v2-retire-v1`, which cannot run
until every v1 surface has a plugin equivalent proven against its recording.
That costs nothing to defer, because the two halves do not conflict.

**Untouched (~45,400 lines).** `agent/` (11,036 — tmux control mode, transport,
PTY reader, vt100, input encoding), `session/` (8,823), `storage/` (6,832),
`cli/` (6,379), `session_ops/` (5,835), `git/` (2,260), plus `paths`, `shell`,
`clipboard`, `notifications`, `sync`, `usage`, `workspace`. The lower half is
already a library with no knowledge of the TUI.

**New.** ~4–6k lines of Rust (plugin host, view tree, layout pass, host API,
registry, surface) and ~2k lines of Lua (widget library, theme, layout, the
bare-core plugins).

**`thurbox-cli` is unaffected**, and therefore so is every shell extension that
drives it — flow, forge, ci-shepherd, renovate and the four task-integration
trackers all keep working with no change.

**Performance work must be re-derived.** ADR-P6 through ADR-P12 (session-order
cache, hook-state cache, non-blocking spawn, capture prefetch, off-thread diff)
all lived in `app/`. The snapshot model obsoletes some outright; the rest need
re-establishing against the new render path, including the demand-driven redraw
and its 250 ms floor.

**A test oracle must be captured before deletion, not after.** Golden recordings
of what each v1 pane draws — asserted cell-exact, with the formatter
destructuring every field by name so a dropped field is a compile error — can
only be taken while the v1 renderers still exist. This is a scheduling
constraint on the change, not an afterthought.

**Documentation.** `docs/ARCHITECTURE.md`, `docs/FEATURES.md`,
`docs/PERFORMANCE.md` and the majority of `CLAUDE.md` describe the deleted half
and are rewritten. A new plugin-authoring guide is required, since the API is
public-but-unstable from day one.

**Risk: a long parity tail.** v2 will not have code review, tasks, automations,
global search or the file viewer until their changes land, and `thurbox2` is not
a replacement for `thurbox` until they do. This is mitigated structurally rather
than by hope: v1 ships unchanged from the same repository throughout, and the
deletion gate is a separate change that names each prerequisite.
