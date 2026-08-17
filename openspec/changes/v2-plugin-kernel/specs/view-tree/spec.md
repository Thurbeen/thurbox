## Purpose

Defines the vocabulary a plugin uses to describe what it wants drawn, and the
contract by which that description becomes painted cells — deliberately small,
so that new appearances are composed in userland rather than added to the
kernel.

## ADDED Requirements

### Requirement: The node vocabulary is limited to four primitives

The view vocabulary SHALL consist of exactly four node kinds: a text node, a box
node that arranges children along an axis and may carry a frame, an input node
that accepts text entry, and a surface node that carries pre-rendered cells.

Appearances that can be composed from these primitives — including lists,
gauges, dividers, tables and titled panels — SHALL NOT be added as node kinds.
They SHALL be provided as a userland library so that introducing one requires no
change to the kernel and no release.

#### Scenario: A composable appearance is needed

- **WHEN** a plugin needs a list, a gauge, a divider or a bordered panel
- **THEN** it composes it from the four primitives, directly or via the userland widget library
- **AND** no new node kind is introduced

#### Scenario: An unknown node kind is returned

- **WHEN** a plugin returns a node naming a kind outside the vocabulary
- **THEN** the tree is rejected as malformed and reported against that plugin

### Requirement: A plugin renders into a resolved rect

The system SHALL resolve the geometry of every plugin's region before invoking
that plugin, and SHALL pass the resolved width and height to the invocation. A
plugin SHALL be able to make layout decisions — wrapping, truncation, splitting,
window sizing — from the dimensions it was given.

#### Scenario: A plugin is given its own region size

- **WHEN** a plugin renders in a region 40 columns wide inside an 200-column screen
- **THEN** the invocation reports a width of 40, not 200

#### Scenario: The region changes size

- **WHEN** the terminal is resized such that a plugin's region changes
- **THEN** the next invocation reports the new dimensions

#### Scenario: A plugin windows a long list

- **WHEN** a plugin holds more rows than its resolved height
- **THEN** it can select the visible window and report overflow from the height it was given

### Requirement: Nodes may carry identity

Any node SHALL be able to carry an identifier, one or more class names, and a
role. Identity SHALL be optional; a node without it renders normally.

Identity SHALL be available for targeting a node — both to attribute an input
event to the node under it, and to allow styling to be expressed separately from
structure. The mechanism that resolves styling rules against identity is out of
scope for this capability.

#### Scenario: A row is given identity

- **WHEN** a plugin marks its rows with a class and a role
- **THEN** the identity is preserved through layout and is available on the painted node

#### Scenario: A node carries no identity

- **WHEN** a node declares no identifier, class or role
- **THEN** it renders normally

### Requirement: Children declare their own size

A child of a box SHALL declare how much of the axis it wants, using an exact
length, a percentage, a proportional share, or a minimum and maximum bound. A
child that declares nothing SHALL receive an equal share of the space remaining
after sized siblings are satisfied.

#### Scenario: Mixed sizing in one box

- **WHEN** a box contains a child of exact length 3, a child of 50 percent, and two undeclared children
- **THEN** the first two receive their requested sizes and the remaining space is split equally between the other two

#### Scenario: Requests exceed the available space

- **WHEN** the declared sizes of children exceed the axis length
- **THEN** the space is allocated deterministically and no child is given a negative size

### Requirement: Colour is named by role, not by value

A plugin SHALL express colour by naming a theme role. The active theme SHALL
resolve a role to a concrete colour. Changing the active theme SHALL change
every plugin's appearance without any plugin being modified.

#### Scenario: The theme changes

- **WHEN** the active theme changes
- **THEN** every plugin that named roles renders in the new palette without being edited

### Requirement: A surface carries geometry-first content

A surface node SHALL accept content as cells rather than as a tree, and the
system SHALL paint it within the node's resolved rect. A surface SHALL be
fillable from a live session's terminal output, and fillable by a plugin that
produces cells itself.

Content that is positioned by character measurement against a resolved width —
including side-by-side comparison, horizontal scrolling and syntax colouring —
SHALL be expressible as a surface.

#### Scenario: A surface is fed by a session

- **WHEN** a plugin places a surface naming a live session
- **THEN** that session's terminal output is painted within the node's rect

#### Scenario: Two surfaces are placed at once

- **WHEN** a layout places two surfaces naming two different sessions
- **THEN** both are painted, each within its own rect

#### Scenario: A surface is fed by a plugin

- **WHEN** a plugin supplies cells directly to a surface
- **THEN** those cells are painted within the node's rect

### Requirement: A malformed tree is reported, not fatal

When a plugin returns a tree the system cannot render — an unknown kind, a
missing required field, a value of the wrong type — the system SHALL report the
failure against that plugin and SHALL continue rendering every other plugin.

#### Scenario: A required field is missing

- **WHEN** a plugin returns a node lacking a field its kind requires
- **THEN** the failure is reported against that plugin and the rest of the screen renders

### Requirement: Repainting is driven by change

The system SHALL paint a frame when the view has changed, when input has been
received, or when a bounded maximum interval since the last paint has elapsed.
An idle application with no input and no changing content SHALL NOT paint at the
input polling rate.

#### Scenario: Nothing changes

- **WHEN** no input arrives and no plugin's view changes
- **THEN** frames are painted at the forced-redraw floor rather than at the polling rate

#### Scenario: A plugin's view changes

- **WHEN** a plugin returns a view differing from its previous one
- **THEN** a frame is painted
