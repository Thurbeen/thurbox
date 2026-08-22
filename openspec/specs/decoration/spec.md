# decoration Specification

## Purpose
Defines how one plugin changes the appearance of nodes another plugin rendered —
the mechanism search needs to highlight matches inside panes it does not own.
## Requirements
### Requirement: A plugin can transform another's rendered tree

The system SHALL allow a plugin to receive a rendered tree belonging to another
plugin and return a modified one, which is what is drawn.

A decorator SHALL be able to find the nodes it cares about by the identity those
nodes carry, without knowing which plugin produced them.

#### Scenario: Matching rows are highlighted

- **WHEN** a decorator restyles nodes carrying a given role
- **THEN** those nodes are drawn restyled, in a pane the decorator does not own

#### Scenario: A pane that carries no identity

- **WHEN** a pane's nodes carry no identity
- **THEN** the decorator finds nothing and the pane is drawn unchanged

### Requirement: A failing decorator costs only its decoration

A decorator that throws SHALL leave the tree it was given drawn as it was, and
SHALL be reported like any other plugin failure.

#### Scenario: A decorator throws

- **WHEN** a decorator raises an error
- **THEN** the undecorated tree is drawn and the failure is reported

### Requirement: Decoration is opt-in and ordered

A plugin SHALL declare that it decorates, and which slot it decorates. Where
several decorate the same slot, they SHALL be applied in a deterministic order.

#### Scenario: Two decorators on one slot

- **WHEN** two plugins decorate the same slot
- **THEN** both are applied, in a deterministic order
