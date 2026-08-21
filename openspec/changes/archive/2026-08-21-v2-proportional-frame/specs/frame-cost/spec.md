## Purpose
Defines what a painted frame is allowed to recompute: that the data a plugin
reads is never stale even when it was not rebuilt, that a reused tree is
indistinguishable from a freshly built one, and that both are observable rather
than asserted — so making a frame cheaper can never be paid for in correctness.

## ADDED Requirements

### Requirement: What a frame recomputes is bounded by what changed

Rebuilding every published table and every pane's tree on every painted frame
costs the same whether anything moved or not. The system SHALL rebuild a
published group, and SHALL run a pane's render, only when something they are
built from has changed since the last painted frame.

This is a bound on *work*, never on *freshness*: the requirements below make
skipping unobservable, and where the two conflict, freshness wins.

#### Scenario: Nothing changed between two frames

- **WHEN** a frame is painted and nothing any published group is built from has
  changed since the previous one
- **THEN** those groups are not rebuilt

#### Scenario: One source moved and the rest did not

- **WHEN** a single source changes between two frames
- **THEN** the groups built from it are rebuilt and the rest are not

#### Scenario: The interface is left alone

- **WHEN** an idle interface with nothing running is left untouched
- **THEN** the frames it paints at the forced-redraw floor rebuild neither the
  published groups nor any pane whose render may be skipped

### Requirement: A plugin never reads a stale published value

A group that was not rebuilt SHALL be indistinguishable, to every reader, from
one that was. A plugin reads the published tables and cannot ask when they were
last written, so a value that is skipped while its source has moved is not a
saving but a wrong answer that no plugin can detect.

#### Scenario: A source changes and a plugin reads it

- **WHEN** something a plugin reads changes
- **THEN** the next painted frame publishes the new value, and a plugin
  rendering in that frame reads it

#### Scenario: A reader compares against a full rebuild

- **WHEN** what a plugin reads on a frame that skipped a rebuild is compared
  against what a full rebuild would have published
- **THEN** they are equal

#### Scenario: A change arrives while the interface is idle

- **WHEN** a source changes while nothing else is happening
- **THEN** the change is published without waiting for unrelated activity to
  wake the loop

### Requirement: A reused tree is indistinguishable from a rebuilt one

Where a pane's render is skipped, the tree it last returned SHALL be painted in
its place, and the painted result SHALL equal what running the render again
would have produced.

#### Scenario: A pane is skipped and painted

- **WHEN** a pane's render is skipped for a frame
- **THEN** the cells painted for it are the ones its last render would have
  produced for that frame

#### Scenario: Something the pane reads changes

- **WHEN** anything a skippable pane's render depends on changes
- **THEN** its render runs again before that frame is painted

#### Scenario: The pane's rect or focus changes

- **WHEN** a pane is given a different rect, or gains or loses focus
- **THEN** its render runs again rather than reusing a tree built for the
  previous one

#### Scenario: A live surface moves under a reused tree

- **WHEN** a pane's tree is unchanged but a terminal surface it draws has
  produced new output
- **THEN** the frame is still painted, because what moved is the surface rather
  than the tree

#### Scenario: The interface is reloaded

- **WHEN** the interface is reloaded from disk
- **THEN** no tree from before the reload is reused

### Requirement: Settling is provable rather than assumed

A loop that quietly stops settling costs a third of a core and looks exactly
like one that works. The system SHALL report how much of a frame it skipped, on
the same counters the rest of the loop is measured by.

#### Scenario: Perf timing is active

- **WHEN** perf timing is active
- **THEN** the published snapshot reports how many published rebuilds and how
  many pane renders were skipped

#### Scenario: An idle interface is measured

- **WHEN** an idle interface is measured over many frames
- **THEN** the skipped counts climb while the rebuilt counts stay flat

#### Scenario: Timing is not active

- **WHEN** perf timing is not active
- **THEN** deciding whether to skip costs no wall-clock reads and publishes
  nothing
