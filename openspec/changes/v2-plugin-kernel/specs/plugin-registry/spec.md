## Purpose

Defines how plugins contribute their keys and settings to one central place
rather than each inventing its own, so that the interface stays coherent as
plugins accumulate and a user can discover and rebind everything from a single
surface.

## ADDED Requirements

### Requirement: Plugins declare their keys as data

A plugin SHALL declare the keys it responds to as data, separately from the code
that handles them. Each declaration SHALL carry the chord, a stable action
identifier, and a human-readable description.

A key that is declared SHALL be enumerable without invoking the plugin.

#### Scenario: A plugin declares keys

- **WHEN** a plugin declares its keys
- **THEN** the chords, actions and descriptions are readable without invoking the plugin

#### Scenario: A plugin is added

- **WHEN** a plugin declaring keys is added
- **THEN** its keys appear in the registry with no other plugin or kernel change

### Requirement: Key declarations are scoped

A key declaration SHALL be scoped either globally or to the declaring plugin.
A plugin-scoped key SHALL take effect only while that plugin holds focus, so
that several plugins may declare the same chord without conflict.

#### Scenario: Two plugins declare the same plugin-scoped chord

- **WHEN** two plugins each declare the same chord scoped to themselves
- **THEN** both declarations are accepted, and each fires only while its plugin holds focus

#### Scenario: A global chord fires from anywhere

- **WHEN** a globally scoped chord is pressed while any plugin holds focus
- **THEN** its action fires

### Requirement: Conflicting declarations are detected and reported

The system SHALL detect when two declarations claim the same chord in
overlapping scopes, SHALL resolve the conflict deterministically, and SHALL
report it to the user identifying both claimants.

#### Scenario: Two global declarations claim one chord

- **WHEN** two plugins declare the same chord globally
- **THEN** the conflict is reported naming both plugins and resolved deterministically

#### Scenario: A global and a plugin-scoped declaration overlap

- **WHEN** a global declaration and a plugin-scoped declaration claim the same chord
- **THEN** the overlap is detected and reported

### Requirement: Plugins declare their settings as data

A plugin SHALL declare the settings it accepts as data, each carrying a stable
identifier, a type, a default value, and a human-readable description. The
system SHALL supply the effective value of each setting to its plugin.

#### Scenario: A plugin declares a setting

- **WHEN** a plugin declares a setting with a default
- **THEN** the plugin receives the default until a value is set

#### Scenario: A setting is given a value

- **WHEN** a value is set for a declared setting
- **THEN** the plugin receives that value in place of the default

### Requirement: User overrides are persisted and applied

The system SHALL allow the chord bound to any declared action to be overridden,
and any declared setting to be given a value. Overrides SHALL persist across
restarts and SHALL be applied in preference to the declared defaults.

An override naming an action or setting that no longer exists SHALL be retained
without effect and SHALL NOT prevent the remaining overrides from applying.

#### Scenario: A binding is overridden

- **WHEN** a user overrides the chord for a declared action
- **THEN** the action fires on the new chord and not the declared one
- **AND** the override survives a restart

#### Scenario: An override is reset

- **WHEN** an override is removed
- **THEN** the declared default applies again

#### Scenario: An override refers to something that no longer exists

- **WHEN** a plugin declaring an overridden action is removed
- **THEN** the override is retained without effect and other overrides still apply

### Requirement: The registry is readable by plugins

The contents of the registry — every declared key with its scope, action,
description, effective chord and declaring plugin, and every declared setting
with its type, default, effective value and declaring plugin — SHALL be readable
by a plugin, so that the surfaces which present and edit them are themselves
plugins.

#### Scenario: A help surface is a plugin

- **WHEN** a plugin renders the list of keys
- **THEN** it can read every declaration from the registry, including those of plugins it does not know about

#### Scenario: A settings surface is a plugin

- **WHEN** a plugin renders and edits settings
- **THEN** it can read every declaration and record overrides through the registry

### Requirement: An escape route is always available

The system SHALL reserve a minimal set of chords that cannot be overridden or
consumed by a plugin, sufficient to move focus, reload plugins and quit.

#### Scenario: A plugin consumes every key

- **WHEN** a focused plugin reports handling every key it receives
- **THEN** the reserved chords still move focus, reload and quit

#### Scenario: A reserved chord is overridden

- **WHEN** an override or declaration claims a reserved chord
- **THEN** it is rejected and reported, and the reserved behaviour is retained
