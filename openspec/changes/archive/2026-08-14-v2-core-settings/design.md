## Context

See `proposal.md` — Why. Two existing shapes constrain the approach.

**v1's settings are half a `OnceLock`.** `session::settings::global()` is written
once at startup; v1 keeps a *second*, mutable copy of the feature flags on `App`
and re-applies it on every frame, which is what makes a flag live. Its panel
edits a draft and writes `settings.toml` on `Ctrl+S`, and it classifies each
field as live or restart-only (`Settings::restart_only_differs`). All of that is
worth keeping — including the classification, which is a *fact about the code*
(mouse capture is enabled once; the notifier thread starts once), not a
preference.

**v2's settings modal is a registry view.** It renders `Registry::settings` —
what plugins declared — and writes straight through, because those values are
in-process. Core settings are a file with restart-only members, so the two cannot
share one write path. They can share one *screen*, which is what the user
actually wants.

## Goals / Non-Goals

**Goals:**

- Every setting v2 exposes is honoured, and the ones it does not implement are
  neither honoured nor advertised.
- One place that owns the live settings and the reload, rather than each call site
  reaching for `global()`.
- v1's live-vs-restart honesty, on screen and on reload.

**Non-Goals:**

- `features.automations` and the pane-less flags — excluded by decision (see the
  proposal). Their values are preserved in the file untouched.
- Reworking `Settings` itself, or its file format. This change reads and writes
  the existing shape.
- A general config capability for plugins. They read the published values; they
  do not write the file.
- Retiring `settings::global()`. Restart-only values are read from it exactly as
  they are today — that is what "restart-only" means.

## Decisions

### D1 — One owner: `kernel::config`, holding the live settings

A new module owns a `Settings` value, its reload, and the question "what changed".
Nothing else re-reads the file, and the loop asks it rather than `global()` for
anything live. The `OnceLock` stays for restart-only reads, so a value that
*cannot* change mid-run is still read where it is used.

*Alternative considered.* Mutating `settings::global()` in place. Rejected: it is
shared with `thurbox-cli` and v1, a write-once global is what makes those callers
safe, and making it mutable would mean every reader silently becomes
time-dependent.

*Consequence.* `Config::features()` is the live answer and `settings::global()` is
the startup answer, and the difference is the point. The two are asserted equal
for restart-only fields by a test, so a field cannot be misclassified without
someone noticing.

### D2 — Writing a core setting is a command; writing a plugin setting stays in-process

`Command::Setting` mutates the registry on the UI thread because that is instant.
Writing `settings.toml` is file I/O — and `save_settings` re-parses the document
to preserve its comments — so it takes the bus like every other thing that
touches the world, as `Command::Bookmark` does. A new `Command::Configure`
carries the whole draft rather than one field: the file is written as a document,
and a per-field command would mean N writes for one save.

*Alternative considered.* Extending `Command::Setting` with a `core.` key
namespace. Rejected: it would give one command two applications (in-process vs a
worker) and two failure modes, and the modal already has a draft to hand over
whole.

### D3 — The modal grows a core section rather than becoming two modals

One screen, two halves, each with the semantics its values demand: the core half
is a draft applied on save with a `⟳` on restart-only rows, the plugin half keeps
writing through. That is a divergence *within* the modal, so it is spelled out on
screen — the core section's footer names the save key, and the plugin section has
none.

*Alternative considered.* A separate "core settings" modal on its own chord.
Rejected: the user asked for the settings they can see in one place, and v1 has
one settings screen.

### D4 — The kernel declares its core rows through the same registry mechanism

The rows are declared as data — id, label, description, kind, live-or-restart —
under a kernel-owned owner, exactly as `kernel::modals` already owns its key
bindings. The renderer then treats core and plugin rows the same, and adding a
core setting is a table entry rather than a rendering change. The declaration
table is v1's `SettingsField::meta()` ported, so the words on screen are the words
v1 shows.

### D5 — Only the settings v2 honours are declared

A row that gates nothing reads as broken, and a flag whose pane does not exist
cannot be honoured. So the declaration table is the honoured set, and a returning
pane brings its own row — which also means the modal cannot drift out of step with
what is implemented: the table *is* the list of what works.

*Consequence, accepted.* `settings.toml` may contain switches the modal does not
show. They are preserved on write (the document is edited, not regenerated), and
`docs/CONFIG.md` remains the full reference.

### D6 — The layout's thresholds come from the published settings

`ui/layout.lua` hardcodes `80`. It becomes
`thurbox.settings.two_panel_min_cols`, with the hardcoded value as the fallback
for a kernel that published nothing — the arrangement must never fail closed.
This is also why the settings are published rather than kept kernel-side: the
arrangement is Lua, and it needs them *before* any plugin runs.

### D7 — The update check and the silent update are workers, and reuse v1's

`version_check::{read_cached_status, cache_is_stale, refresh_cache}` and
`self_update::perform_update` already exist and are what v1's TUI drives. The
kernel reads the cached status at startup (instant, no network), refreshes it on a
worker when stale, and runs the silent update on a worker at startup — skipped for
a dev build, as v1 skips it. The band already has an `update_available` field
waiting for a value.

### D8 — Reload is a poll, and only reports what it cannot apply

v1 watches the file's mtime; the same poll is added beside the plugin-directory
watcher already in the loop. A reload re-applies live settings silently — the
point is that they are live — and reports only when a restart-only value moved,
which is the case the user would otherwise misread.

## Risks / Trade-offs

**A field classified live that is not.** Turning mouse capture off mid-run means
not sending an escape the terminal was already told; turning it on means sending
one. → The classification is v1's, unchanged, and asserted by a test against
`restart_only_differs` so the two cannot drift.

**The modal writes a file the user may be editing.** → `save_settings` writes
through `toml_edit`, preserving comments and unknown keys, and the reload poll is
told about our own write (v1's `mark_settings_saved`) so a save does not toast
itself as an external change.

**Two write paths in one screen** is a real complexity cost. → It is v1's
semantics for v1's reasons, stated on screen; the alternative is either losing
restart honesty or splitting the screen the user asked to unify.

**`auto_update` replaces binaries.** → Untouched from v1: same function, same
dev-build skip, same "restart to apply" report. This change only decides *whether*
it runs, from the same flag v1 reads.

**Publishing settings grows what a plugin can read.** → It is a read of values the
user wrote; no capability is added, and the sandbox declaration is updated in the
same commit so a typo is a lint error.

## Findings from implementing

**The whole change turned out to rest on one missing line.** The proposal said v2
honoured five settings and ignored the rest. It honoured *none*: `thurbox2` never
called `settings::init`, and `settings::global()` is documented to hand out
`Settings::default()` when it was not initialised. Every switch looked honoured
because every default matched — mouse capture on, notifications on, 90-day audit
retention — so the failure was invisible from the outside and stayed that way
through a whole feature's worth of work. Two settings (`audit_retention_days`,
`scrollback_lines`) needed no code at all beyond that line: the code that reads
them was already correct and already running. `tests/v2_core_settings.rs` opens by
asserting the *reach* of a setting rather than its parsing, because parsing was
never the thing that was broken.

**A confirmation surface was a prerequisite, not a nicety.** `soft_delete = false`
requires a confirmation before an irreversible delete. v2 had none: the session
list already wrote `store.confirm`, but the pane that consumed it was cut in the
interface cutback, so `D` (force delete) had been silently doing nothing.
`60_confirm.lua` came back to satisfy this change and fixed that key on the way.

**Two write paths in one screen justified themselves quickly.** The plugin half
writes through on the keystroke; the core half is a draft saved with `Ctrl+S`.
Sharing one renderer was the right call — the core rows are synthesised as
ordinary `Setting`s, so one row renderer draws both halves — but `Registry` was
*not* the right home for their values: `set_setting` persists to the registry's
own JSON, which would have made `settings.toml` and `ui.json` two sources of
truth for the same switch. The draft lives on the modal instead, and the only
place the halves differ is one `put` that branches on the owner.

**The live/restart split had to be spelled out field by field**, because
`Settings` has no marker for it. `Config::adopt` names each field explicitly and
is guarded two ways: a test that flips every live flag and demands they all take,
and a test that checks the classification against `restart_only_differs` — the
predicate v1 already had. A new flag added to the wrong half fails both.

**An unrelated hazard surfaced while verifying.** Ten test helpers commit into
throwaway repositories, setting a local identity but not disabling signing — so a
developer whose global config signs commits needs a key loaded in their agent for
the "fully hermetic" acceptance suite to pass. Fixed where it was found
(`commit.gpgsign = false` per test repo), noted here because the next helper that
creates a repository will want the same line.
