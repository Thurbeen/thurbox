## MODIFIED Requirements

### Requirement: What a frame recomputes is bounded by what changed

Rebuilding every published table and every pane's tree on every painted frame
costs the same whether anything moved or not. The system SHALL rebuild a
published group, and SHALL run a pane's render, only when something they are
built from has changed since the last painted frame. An event handler SHALL
run only when the signal it is derived from changed, and a handler that writes
nothing SHALL mark nothing dirty: a `pure` pane is invalidated by a handler only
through the `state` and `store` writes it already keys on.

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
  published groups nor any pane whose render may be skipped, and no event
  handler runs

#### Scenario: A handler observes without writing

- **WHEN** an event is delivered to a handler that reads the payload and
  writes neither `state` nor `store` nor enqueues a command
- **THEN** the frame is not marked dirty by the dispatch, and every `pure`
  pane's tree is reused

#### Scenario: A handler writes state

- **WHEN** a handler writes `state`
- **THEN** the next frame is painted and the panes keyed on that state are
  rebuilt, exactly as a key handler's write would cause
