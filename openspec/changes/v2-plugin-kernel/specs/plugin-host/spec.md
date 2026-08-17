## Purpose

Defines how Lua plugins are discovered, loaded, reloaded and isolated, so that
editing a plugin takes effect without restarting thurbox and a broken or
misbehaving plugin degrades only its own panel rather than the application.

## ADDED Requirements

### Requirement: Plugins are discovered from a plugin directory

The system SHALL load every `*.lua` file in the active plugin directory as a
plugin. A plugin SHALL be able to declare its name, the slot it renders into,
its ordering among peers, and whether it joins the focus ring. Where a
declaration is absent the system SHALL apply a documented default rather than
rejecting the plugin.

#### Scenario: A new plugin file is added

- **WHEN** a `*.lua` file is placed in the plugin directory
- **THEN** it is loaded on the next reload and renders into its declared slot

#### Scenario: A plugin omits optional declarations

- **WHEN** a plugin declares neither a slot nor an ordering
- **THEN** the documented defaults are applied and the plugin loads normally

### Requirement: A reload rebuilds the plugin environment wholesale

The system SHALL reload plugins by constructing a fresh environment and loading
every plugin into it, rather than by patching the running one. Cached modules
SHALL be discarded so shared libraries reload alongside the plugins that require
them.

#### Scenario: A shared library changes

- **WHEN** a file under the shared library directory is edited and a reload occurs
- **THEN** every plugin that requires it observes the new version

### Requirement: A failed reload preserves the running environment

When constructing the new environment fails for any reason — a syntax error, a
plugin that throws while loading, an unreadable file — the system SHALL discard
the partial environment, continue running the last environment that loaded
successfully, and surface the failure to the user with the originating file and
message.

#### Scenario: A plugin is saved with a syntax error

- **WHEN** a plugin file is saved containing a syntax error
- **THEN** the previously loaded plugins keep rendering and keep their state
- **AND** the error is displayed with the file that caused it

#### Scenario: The error is corrected

- **WHEN** the file is saved again without the error
- **THEN** the new environment replaces the running one and the error clears

### Requirement: Reload is triggered by saving and on demand

The system SHALL reload automatically within a bounded time of a change to any
file in the plugin directory tree, tolerating the burst of filesystem events a
single editor save produces. The system SHALL also expose an explicit
reload-now action.

#### Scenario: An editor writes a file in several operations

- **WHEN** a save produces a rapid sequence of write, rename and permission events
- **THEN** exactly one reload occurs

### Requirement: A failure during render is isolated to its own panel

When a plugin fails while producing its view, the system SHALL render an error
panel in that plugin's own region carrying the failure message, and SHALL
continue rendering every other plugin normally. A failing plugin SHALL NOT cause
its neighbours to lose state or stop updating.

#### Scenario: One plugin of several throws while rendering

- **WHEN** one plugin in a multi-plugin screen raises an error during render
- **THEN** an error panel replaces only that plugin's region
- **AND** the other plugins render normally and retain their state

### Requirement: A failure during key handling is isolated and reported

When a plugin fails while handling a key, the system SHALL report the failure
without terminating the application, and SHALL continue dispatching subsequent
keys.

#### Scenario: A plugin throws on a keypress

- **WHEN** a focused plugin raises an error while handling a key
- **THEN** the failure is surfaced to the user and the application keeps running
- **AND** the next keypress is dispatched normally

### Requirement: Plugin state survives reloads

The system SHALL provide each plugin with persistent private storage that is not
visible to other plugins, and one shared store visible to all plugins. Both
SHALL survive a reload. Both SHALL accept nil, booleans, numbers, strings and
tables, and SHALL preserve the distinction between integer and fractional
numbers.

#### Scenario: A plugin is edited while holding state

- **WHEN** a plugin holding a selection index is edited and reloaded
- **THEN** the selection index is unchanged after the reload

#### Scenario: Two plugins use the same private key

- **WHEN** two different plugins each store a value under the same private key
- **THEN** neither observes the other's value

#### Scenario: A plugin publishes to the shared store

- **WHEN** one plugin writes a value to the shared store
- **THEN** another plugin reads that value

### Requirement: Plugin execution time is bounded

The system SHALL abort a plugin invocation that exceeds a bounded execution
budget, report it as a plugin failure, and remain responsive to input. A plugin
containing an unterminated loop SHALL NOT render the application unusable.

#### Scenario: A plugin enters an infinite loop during render

- **WHEN** a plugin's render does not terminate
- **THEN** the invocation is aborted and reported as that plugin's failure
- **AND** the application continues to accept input and render other plugins

### Requirement: Plugin memory is bounded

The system SHALL enforce an upper bound on memory allocated by the plugin
environment. When the bound is exceeded, the allocation SHALL fail as a plugin
error rather than terminating the process.

#### Scenario: A plugin allocates without bound

- **WHEN** a plugin allocates memory until the bound is reached
- **THEN** the failure is reported as a plugin error and the application survives

### Requirement: Plugin failures are attributable

Every reported plugin failure SHALL identify the plugin responsible and the
phase in which it occurred (load, render, or key handling).

#### Scenario: A failure is reported

- **WHEN** any plugin failure is surfaced
- **THEN** the report names the plugin and the phase
