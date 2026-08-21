## ADDED Requirements

### Requirement: A pane may declare its render pure, and is then not called every frame

A pane's render may write to shared state, and may animate from a per-frame
clock. Neither is visible from outside the plugin, so the kernel cannot decide
on its own that skipping a render is safe. A pane MAY therefore **declare** that
its render is a function of the published tables and its render context and
nothing else; the system SHALL skip the render of a pane that has declared this
while those inputs are unchanged, and SHALL reuse the tree it last returned.

The declaration is an assertion the author makes, not a property the kernel
checks. A pane that declares it and then writes shared state, reads a clock, or
depends on anything it was not given will be painted from a stale tree, and the
symptom is a pane that stops updating rather than an error.

#### Scenario: A declared-pure pane's inputs are unchanged

- **WHEN** a pane that has declared its render pure is drawn on a frame where
  nothing it can read has changed
- **THEN** its render is not called, and the tree it last returned is painted

#### Scenario: A pane does not declare purity

- **WHEN** a pane makes no such declaration
- **THEN** its render is called on every frame it is drawn on, exactly as
  before the declaration existed

#### Scenario: A pane written by someone else is loaded

- **WHEN** a plugin written against the interface as it was before is loaded
- **THEN** it renders every frame and behaves identically, having declared
  nothing

#### Scenario: A pure pane animates at the rate the interface animates at

- **WHEN** a pane animates from the elapsed time it is handed, at the rate the
  shared widgets advance an animation
- **THEN** it may still declare its render pure, and the animation is unaffected:
  a reused tree is dropped exactly when that animation would move to its next
  frame, rather than merely because time passed

#### Scenario: A pure pane needs a per-frame clock

- **WHEN** a pane animates from the frame number it is handed, or from elapsed
  time at a finer granularity than the shared animation rate
- **THEN** it is not eligible to declare its render pure, and the written guide
  says so where the declaration is described

#### Scenario: A pure pane writes to shared state while rendering

- **WHEN** a pane writes to the shared store from inside its render
- **THEN** it is not eligible to declare its render pure, because the write
  would stop happening on the frames its render is skipped

#### Scenario: The declaration is misspelled

- **WHEN** a pane's declaration table carries a misspelling of the purity key
- **THEN** the pane is treated as not having declared it, and so renders every
  frame exactly as it did before — the failure of a misspelling is a pane that
  is merely no faster, never one that is stale

This is why the declaration is opt-in rather than opt-out. No key of a
declaration table is checked today — `slot`, `focusable` and `order` are equally
unchecked, and a plugin may carry its own bookkeeping there — so a misspelling
cannot be reported without inventing a schema for the whole table. Opt-in makes
that acceptable: the unchecked direction is the safe one.

#### Scenario: A pure pane is edited on disk

- **WHEN** a pane is edited and the interface reloads
- **THEN** the reloaded pane renders afresh rather than being painted from the
  tree its previous version returned
