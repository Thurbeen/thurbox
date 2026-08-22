## Purpose

Defines the persistent single-row bands that report the application's own state —
what it is, what just happened, and what can be pressed — so that this chrome has
exactly one owner, cannot be broken by the panes it sits beside, and still grows
an entry when a new pane is added.

## ADDED Requirements

### Requirement: A band reports the application, not the user's work

A chrome band SHALL show state belonging to the application — its identity, its
messages, the actions it offers — and SHALL NOT show the contents of a session.

A band SHALL NOT be focusable and SHALL NOT receive keyboard input. Moving focus
forward from the last focusable surface SHALL reach the first focusable surface,
never a band.

#### Scenario: Focus is cycled through every surface

- **WHEN** focus is moved forward repeatedly
- **THEN** every focusable surface is visited and no band is ever focused

#### Scenario: A key is pressed while a band is visible

- **WHEN** any key is pressed
- **THEN** the key is offered to the focused surface and never to a band

### Requirement: The arrangement places bands, the application fills them

A band SHALL be positioned by the arrangement, named the same way a pane's
region is named. The arrangement SHALL decide whether a band appears, where it
appears, and in what order relative to the panes.

The arrangement SHALL NOT determine a band's contents.

A band the arrangement does not place SHALL NOT be drawn, and its absence SHALL
NOT be an error.

#### Scenario: A band is omitted from the arrangement

- **WHEN** the arrangement does not place a band
- **THEN** that band does not draw, the remaining bands and panes are unaffected, and no error is reported

#### Scenario: A band is moved

- **WHEN** the arrangement places a band in a different position
- **THEN** the band draws in that position with the same contents

#### Scenario: The screen is too short for every band

- **WHEN** the available height cannot accommodate every placed band and a usable pane area
- **THEN** bands are dropped in a documented order, the pane area is never reduced below one row, and the message band is retained in preference to the identity band

### Requirement: A band cannot be broken by a plugin

Drawing a band SHALL NOT invoke plugin code.

A plugin that fails while rendering, or while handling input, SHALL NOT prevent
any band from drawing and SHALL NOT change what any band shows.

Failing to **load** is a different case, because plugins are loaded as a set: the
set that was already in force SHALL remain in force — its contributions included
— and the failure SHALL be reported. Where no set has loaded yet, the built-in
one SHALL be used. A band therefore never shows a half-loaded set, and a broken
file never silently removes an entry.

#### Scenario: A contributing plugin raises an error while rendering its own pane

- **WHEN** a plugin fails during its own render
- **THEN** every band still draws, and the plugin's contributed entries still appear

#### Scenario: A plugin file becomes unloadable

- **WHEN** a plugin can no longer be loaded and the set is reloaded
- **THEN** the previously loaded set stays in force with its entries, every band still draws, and the failure is reported

### Requirement: The identity band reports what is running

The identity band SHALL show the product name and the running version.

It SHALL show the active theme and the selected session when there is one.

When a newer release is known to be available, it SHALL say so and name that
version.

#### Scenario: The application is running a known version

- **WHEN** the identity band is drawn
- **THEN** it shows the product name and the running version

#### Scenario: A newer version is available

- **WHEN** a newer release has been detected
- **THEN** the band names the available version and distinguishes it from the running one

#### Scenario: No session is selected

- **WHEN** no session is selected
- **THEN** the band still shows the product, version and theme, and omits the session

### Requirement: The message band reports what just happened

The message band SHALL carry a single message with a severity of informational,
success, or error, and SHALL distinguish the three visibly.

The band SHALL occupy space only while there is a message to show.

A message SHALL be retired after a bounded time, after which the band SHALL
release its space.

Work that outlives that bound SHALL be reported as progress rather than as a
message, and SHALL remain visible for as long as the work runs.

#### Scenario: An action reports an error

- **WHEN** an operation fails
- **THEN** the message band appears carrying the failure at error severity

#### Scenario: A message expires

- **WHEN** the retention time elapses
- **THEN** the message is removed and the band releases the row it occupied

#### Scenario: Long-running work is started

- **WHEN** work begins that will outlast the message retention time
- **THEN** its progress is shown for the duration of the work rather than expiring partway through it

#### Scenario: A second message arrives

- **WHEN** a message arrives while one is shown
- **THEN** the newer message is shown and the older one does not reappear

### Requirement: The action band offers what can be pressed

The action band SHALL present the actions reachable from the current context as
entries, each naming the action and the chord that currently invokes it.

An entry SHALL show the chord actually in force, including one the user has
rebound.

Pressing an entry SHALL perform the same action as pressing its chord.

Entries SHALL be ordered by a declared priority, and when the available width is
insufficient the lowest-priority entries SHALL be dropped rather than truncated
or overlapped.

#### Scenario: An action has been rebound

- **WHEN** an action's chord has been changed by the user
- **THEN** its entry shows the new chord

#### Scenario: An entry is pressed

- **WHEN** an entry is activated by pointer
- **THEN** the same action runs as when its chord is pressed

#### Scenario: The band is too narrow for every entry

- **WHEN** the width cannot fit every entry
- **THEN** the highest-priority entries remain fully legible and the rest are omitted

### Requirement: A plugin contributes an entry by declaring it

A plugin SHALL be able to contribute an action-band entry by declaring it as
data, carrying at least the action it invokes, its label, and its priority.

A contributed entry SHALL appear with no edit to any other plugin, to the
arrangement, or to the bands themselves.

A declaration SHALL be enumerable without invoking the plugin that made it.

An entry naming an action that is not declared anywhere SHALL NOT be shown, so
that the band never offers an affordance that would do nothing.

#### Scenario: A plugin declaring an entry is added

- **WHEN** a plugin that declares an entry is added
- **THEN** its entry appears in the action band with nothing else edited

#### Scenario: A plugin is removed

- **WHEN** a plugin that declared an entry is removed
- **THEN** its entry disappears, and the remaining entries close the gap

#### Scenario: An entry names an action nobody declares

- **WHEN** an entry names an action that no plugin or system surface declares
- **THEN** the entry is not shown

### Requirement: Live values are read from the application, not contributed

Values in a band that change as the application runs — counts of sessions and
scheduled work, the progress of work in flight, the name of the focused surface,
the running version, the active theme — SHALL be read from the application's own
state.

A contributor SHALL NOT be required to supply them, and SHALL NOT be able to
misreport them.

#### Scenario: A count changes

- **WHEN** the number of sessions changes
- **THEN** the band reflects the new count without any plugin declaring or publishing it

#### Scenario: Focus moves

- **WHEN** focus moves to another surface
- **THEN** the band names the newly focused surface
