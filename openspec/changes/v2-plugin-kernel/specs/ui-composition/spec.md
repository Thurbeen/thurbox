## Purpose

Defines how independently written plugins compose into one coherent screen —
where each renders, which is visible, which receives keys, and how a plugin can
float above the rest — so that adding a plugin does not require editing the ones
already there.

## ADDED Requirements

### Requirement: Plugins render into named slots

A plugin SHALL declare the slot it renders into. Several plugins MAY declare the
same slot. An arrangement SHALL position slots within the screen; it SHALL
receive the plugins' declarations and SHALL NOT invoke the plugins itself, so
that a fault in the arrangement cannot break a plugin and a fault in a plugin
cannot break the arrangement.

#### Scenario: A plugin is added to an occupied slot

- **WHEN** a plugin declaring an already-occupied slot is added
- **THEN** it appears in that slot alongside the existing plugins, with no other plugin edited

#### Scenario: The arrangement fails

- **WHEN** the arrangement raises an error
- **THEN** the failure is reported and individual plugins are unaffected

### Requirement: A slot has a mode

A slot SHALL declare whether its occupants are stacked or switched.

In a stacked slot, every occupant is visible simultaneously and shares the
slot's space according to each occupant's declared size.

In a switched slot, exactly one occupant is visible at a time, and the system
SHALL expose which occupants are available and which is active so that a plugin
can render a selector for them.

#### Scenario: A stacked slot holds several plugins

- **WHEN** two plugins occupy a stacked slot
- **THEN** both render, sharing the slot's space by their declared sizes

#### Scenario: A switched slot holds several plugins

- **WHEN** three plugins occupy a switched slot
- **THEN** exactly one renders, and the set of occupants and the active one are readable

### Requirement: The active occupant of a switched slot can be selected

The system SHALL allow the active occupant of a switched slot to be selected
explicitly, and SHALL remember that selection.

A plugin MAY additionally declare that it takes the active position of a
switched slot while it holds focus, and the system SHALL restore the previously
selected occupant when it loses focus.

#### Scenario: The user selects an occupant

- **WHEN** an occupant of a switched slot is selected
- **THEN** it becomes the visible one and remains so until another is selected

#### Scenario: A plugin claims the slot on focus

- **WHEN** a plugin declaring focus-claim over a switched slot gains focus
- **THEN** it becomes the visible occupant

#### Scenario: The claiming plugin loses focus

- **WHEN** that plugin loses focus
- **THEN** the previously selected occupant becomes visible again

### Requirement: Focus identifies which plugin receives keys

Exactly one plugin SHALL hold focus at a time, among those that declared
themselves focusable. The system SHALL provide actions to move focus between
them, and SHALL report to each plugin whether it currently holds focus so it can
render the distinction.

Focus SHALL NOT rest on a plugin that is not currently visible. When the focused
plugin ceases to be visible, focus SHALL move to a visible one.

#### Scenario: Focus moves

- **WHEN** the focus-forward action is invoked
- **THEN** focus moves to the next focusable plugin and both plugins observe the change

#### Scenario: The focused plugin becomes hidden

- **WHEN** the plugin holding focus stops being visible
- **THEN** focus moves to a visible focusable plugin

#### Scenario: A plugin is removed while focused

- **WHEN** a reload removes the plugin that held focus
- **THEN** focus resolves to a plugin that still exists

### Requirement: Keys are dispatched in a defined order

A keypress SHALL be offered first to any plugin holding an exclusive grab, then
to the focused plugin, then to plugins that declared themselves non-focusable
listeners, until one reports that it handled the key. A key no plugin handles
SHALL fall through to the system's own bindings.

#### Scenario: The focused plugin handles a key

- **WHEN** the focused plugin reports handling a key
- **THEN** no other plugin and no system binding receives it

#### Scenario: The focused plugin declines a key

- **WHEN** the focused plugin reports not handling a key
- **THEN** the key continues to the remaining listeners and then to the system bindings

#### Scenario: Two plugins use the same key

- **WHEN** two focusable plugins both handle the same key
- **THEN** only the focused one receives it

### Requirement: A plugin can float above the screen and grab keys

A plugin SHALL be able to declare that it renders above the rest of the screen,
and that it takes every key while it does so. The system SHALL render such a
plugin over the arrangement, obscuring what is beneath without disturbing it,
and SHALL route all input to it.

Where several float simultaneously, the system SHALL order them deterministically
and route input to the topmost.

#### Scenario: A plugin floats

- **WHEN** a plugin declares itself floating and exclusive
- **THEN** it renders above the arrangement and receives every key
- **AND** the plugins beneath retain their state

#### Scenario: The float is dismissed

- **WHEN** the floating plugin stops declaring itself floating
- **THEN** the screen beneath is revealed unchanged and input returns to the previously focused plugin

#### Scenario: Two plugins float at once

- **WHEN** two plugins declare themselves floating simultaneously
- **THEN** their order is deterministic and the topmost receives input

### Requirement: The arrangement is responsive to screen size

The arrangement SHALL be able to vary with the dimensions available and SHALL
be expressed in userland, so that the size thresholds at which regions appear
and disappear can be changed without modifying the kernel.

#### Scenario: The screen is narrowed below a threshold

- **WHEN** the terminal is narrowed past a threshold the arrangement defines
- **THEN** the arrangement produces its narrower form

#### Scenario: A threshold is changed

- **WHEN** a user edits the size thresholds
- **THEN** the new thresholds take effect on the next reload, with no kernel change
