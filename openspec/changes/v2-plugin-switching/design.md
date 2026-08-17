# v2-plugin-switching — Design

## Context

See `proposal.md` — Why. Three existing mechanisms shape where this fits, and
the whole design is about not becoming a fourth:

- **Delivery** (`kernel::bundled`) owns what is *on disk*: it writes shipped
  files, preserves edits, and tombstones removals in `.bundled.json`. Whether a
  file is loaded is not its business.
- **Trust** (`kernel::registry`, persisted in `ui.json`) owns a per-file decision
  the user made, keyed by absolute path, read at load. Disabling is the same
  shape of thing: a decision about a file, not a fact about it.
- **The Interface tab** (`kernel::modals::interface`) is where a file's state is
  read and acted on, and it already carries `restore` and `remove`.

The load path matters: `LuaHost::build` walks `plugins/*.lua`, loads each, and a
plugin that loads contributes its declarations to the registry. "Not loaded" is
therefore expressible exactly once — by not reading the file — and everything the
spec asks for (no keys, no settings, no slot, no capability) follows from that
rather than needing to be enforced separately.

## Goals / Non-Goals

**Goals:**

- Wanting a pane gone never risks the file.
- The reversible action is the reflex; the destructive one is not reachable by
  the same one.
- "Disabled" costs nothing to express and nothing to check.

**Non-Goals:**

- Disabling from outside the running interface. `plugin list` will *report* it,
  but a CLI verb to set it is a separate surface with its own questions (which
  interface directory? whose decision?).
- Any notion of a plugin package, bundle or dependency. A file is the unit.
- Rescuing a file already deleted. This change prevents the loss; it does not
  add a trash can (see D5).

## Decisions

### D1 — Disabled is a decision, stored with the other decisions

The set of disabled paths lives in `ui.json` beside trust and the key
rebindings, keyed by **absolute** path.

*Why not the delivery manifest:* `.bundled.json` records what the *binary* did to
the directory — written, updated, preserved, tombstoned. A disabled bundled file
is still one delivery should keep up to date, and putting a user preference in
the manifest would make "the user turned this off" and "we stopped shipping this"
the same kind of fact. They are not.

*Why absolute:* a repo's `./ui` and the config directory's are different sets of
files. Trust already made this decision for the same reason.

### D2 — Not loading is the whole implementation

`LuaHost::build` skips a disabled file. Nothing downstream needs to know: the
plugin is not in `plugins`, so it declares no keys, occupies no slot, has no
capability installed and cannot fail to load.

*Why this rather than a `loaded but inactive` flag:* every alternative means
teaching each consumer — the registry, the layout, the focus ring, the
capability gate — a second way for a plugin to be absent. The spec's five
"inert" scenarios are all one consequence of one decision, which is what makes
them cheap to hold.

*Consequence, accepted:* a disabled plugin's *file-level* problems are invisible
while it is off — a syntax error in it is not reported until it is turned back
on. That is correct: it is not running, and reporting failures for files nobody
asked to run is how the inventory fills with noise.

### D3 — The host is told, the same way it is told about trust

`LuaHost::set_disabled(paths)` mirrors `set_trusted`: the loop reads the decision
from the registry and hands it over, because the host has no business opening the
user's preference file. A change re-runs the reload that trust already triggers.

*Why reuse the reload:* toggling has to take effect without a restart, and a
reload is exactly "build the VM from what is on disk *and* what the user
decided". Trust proved the path; a second mechanism would be a second thing to
keep in step.

### D4 — The keys: `space` toggles, `d` deletes

`space` turns a plugin off or on, with no confirmation. `d` deletes, with the
existing two-press arm. The Interface tab's rows already respond to `j`/`k`,
`r`, `d` and `t`.

*Why `space`:* it is the pane's own "act on this row" key elsewhere in the
interface (the automations pane toggles an automation with it), it is nowhere
near `d`, and it costs nothing when pressed by accident — which is the property
that matters most here.

*Why no confirmation on the toggle:* a confirmation on a reversible action
teaches people to confirm without reading, which is exactly what makes the
irreversible one dangerous.

### D5 — The warning is worded from what the system knows, not from a guess

The confirmation asks `Source`: a file the binary ships is described as
restorable, a file it does not as one with no copy. That is the same fact
`restore` already reports after refusing.

*Rejected — a trash can.* Moving a removed file to a recoverable location was the
obvious alternative to warning about it. It fails on ownership: the interface
directory is the user's, a hidden `.trash` in it is litter thurbox never cleans
up, and anywhere else is a second location for interface files to live. The
honest fix for "I might not have meant that" is to *ask properly*, and the
reversible action removes most of the reasons anyone reaches for delete at all.

*Rejected — refusing to delete a user's own file.* It would make the interface
unable to undo the thing a user most wants undone (a file they added), and would
push them to `rm` outside thurbox, where the manifest is not updated.

### D6 — `disabled` is a state, not a flag beside a state

`inventory::State` gains a variant rather than `Row` gaining a `bool`. The
question the list answers is "why is this not on screen", and it has one answer
per row: failed, removed, disabled, no slot, hidden, on screen.

*Consequence:* the ordering that puts trouble first (`failed`, `removed`) gets a
new entry, and disabled belongs with the *chosen* states rather than the faults —
it is not a problem to be fixed.

## Risks / Trade-offs

- **A disabled plugin's key looks free until it is re-enabled** → turning it back
  on can now surface a conflict that did not exist while it was off. Correct, and
  the registry already reports conflicts; the alternative (reserving keys for
  plugins that are not running) would make disabling not-quite-absence.
- **Two decisions now live in `ui.json` keyed by path** (trust, disabled) → they
  will drift if a file is renamed, silently forgetting both. Same exposure trust
  already has; a rename is indistinguishable from a delete plus an add at this
  layer.
- **`space` in the Interface tab is one more key on a row that has four** → the
  tab's hint line has room, and the alternative (a modifier, or a submenu) buys
  nothing for the one action that should be effortless.

## Migration Plan

Additive. Nothing is disabled by default, so an existing interface behaves
identically. `ui.json` gains a key; an older binary reading a newer file ignores
it, and a newer binary reading an older one finds nothing disabled. Rollback is
turning things back on — or deleting the key, which has the same effect.

## Open Questions

None. The wording of the two confirmations is worth reviewing against the real
screen rather than deciding here, but it changes no requirement and no task.
