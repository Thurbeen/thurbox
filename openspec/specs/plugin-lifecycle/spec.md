# plugin-lifecycle Specification

## Purpose
Defines what a user may do to the interface they were shipped — add, remove,
replace and edit every kind of plugin, including the bundled ones — and what the
system owes them in return: a delivery mechanism that never overwrites their
work, a removal that survives an upgrade, a way to undo one, and visibility into
which plugins are present and which of them are actually running.
## Requirements
### Requirement: Every kind of plugin is added by adding a file

The interface SHALL be extended by placing a file in the plugin directory, with
no build step, no registration elsewhere, and no restart. This SHALL hold for
every kind of plugin: a pane, a decorator of another pane, a float/modal, and a
shared module loaded by `require`.

#### Scenario: A pane is added

- **WHEN** a user adds a plugin file declaring a slot the arrangement places
- **THEN** it renders in that slot on the next reload, and joins the focus ring if it is focusable

#### Scenario: A decorator is added

- **WHEN** a user adds a plugin that decorates an existing pane
- **THEN** it decorates that pane with no edit to the pane it decorates

#### Scenario: A shared module is added

- **WHEN** a user adds a module file and another plugin requires it
- **THEN** the module loads, and delivery of the bundled interface neither overwrites nor removes it

### Requirement: The arrangement is a user file

The arrangement SHALL be an editable file in the plugin directory. Adding,
removing, moving or resizing a region SHALL require no change to the binary.

#### Scenario: A slot is added

- **WHEN** a user adds a slot to the arrangement and a plugin declares it
- **THEN** that plugin is placed in the new region on the next reload

#### Scenario: A slot is removed

- **WHEN** a user removes a slot from the arrangement
- **THEN** plugins declaring it do not draw, do not take focus, and do not fault

### Requirement: Deleting a bundled plugin removes it

Deleting a bundled file from the plugin directory SHALL remove it from the
interface permanently. The system SHALL NOT restore a deleted bundled file on a
later start, nor on an upgrade to a version that carries a different copy of it.

The record of the removal SHALL live with the plugin directory, so that copying
or discarding that directory carries or discards the removals with it.

#### Scenario: A bundled pane is deleted

- **WHEN** a user deletes a bundled plugin file and restarts
- **THEN** the file is not written again and the pane is absent, together with its slot occupancy, its keys and its contributed entries

#### Scenario: An upgrade follows a deletion

- **WHEN** a newer binary runs and one of its bundled plugins was previously deleted by the user
- **THEN** the deletion stands and the file is not written

#### Scenario: The plugin directory is discarded

- **WHEN** a user deletes the whole plugin directory and restarts
- **THEN** the full bundled interface is delivered again, including plugins previously removed

### Requirement: A bundled plugin can be replaced by a differently named file

A user SHALL be able to supply their own implementation of a bundled pane under
a filename of their choosing, and end up with exactly one of them.

#### Scenario: A session list is replaced

- **WHEN** a user adds their own session-list plugin and deletes the bundled one
- **THEN** only the user's version loads, occupies the slot and receives the keys it declares

### Requirement: Delivery never writes over the user's work

The delivery of the bundled interface SHALL write only files the system itself
ships. It SHALL NOT create, modify or delete any other file in the plugin
directory, and SHALL NOT modify a shipped file the user has since edited.

#### Scenario: An upgrade meets an edited bundled plugin

- **WHEN** a newer binary runs and a bundled plugin has been edited by the user
- **THEN** the user's version is preserved, is what loads, and the difference is reported

#### Scenario: An upgrade meets user-written files

- **WHEN** a newer binary runs and the plugin directory holds files the system never shipped
- **THEN** those files are untouched

### Requirement: A removal or an edit can be undone

The system SHALL be able to restore any bundled plugin from the copy embedded in
the binary — both one the user deleted and one the user edited. Restoring SHALL
be per file, SHALL take effect without a restart, and SHALL be offered from
inside the running application.

#### Scenario: A removed plugin is restored

- **WHEN** a user restores a plugin they had deleted
- **THEN** the shipped file is written again, the removal record is cleared, and the pane returns on the next reload

#### Scenario: An edited plugin is reset

- **WHEN** a user resets a bundled plugin they had edited
- **THEN** their version is replaced by the shipped one and the pane reflects it on the next reload

### Requirement: The interface reports its own inventory

The system SHALL make available, to plugins, an inventory of the interface: for
each plugin its name, file path, declared slot, whether it came from the binary
or from the user, whether a shipped file has been edited or removed, and whether
it is currently running.

The system SHALL also report which directory the interface was loaded from.

#### Scenario: The inventory is read

- **WHEN** a plugin reads the inventory
- **THEN** it receives one entry per plugin, each carrying its path, slot, source and state

#### Scenario: The source directory is shadowed

- **WHEN** the interface is loaded from a directory other than the user's own copy
- **THEN** the directory in use is reported, so the shadowing is visible rather than silent

### Requirement: A plugin that is present but not running is visible

A plugin that exists on disk and does not appear on screen SHALL be reported,
with the reason. The reasons SHALL at least distinguish: it failed to load; it
declares a slot the arrangement does not place.

#### Scenario: A plugin declares an unplaced slot

- **WHEN** a plugin's declared slot is not placed by the arrangement
- **THEN** it is reported as present and unplaced, rather than silently dropped

#### Scenario: A plugin fails to load

- **WHEN** a plugin fails to load and an earlier version of it keeps running
- **THEN** it is reported as failed, with its error, so the running interface is not mistaken for the file on disk

### Requirement: The inventory surface is a plugin like any other

The surface presenting the inventory SHALL be an ordinary bundled plugin: it
SHALL hold no capability unavailable to a user-written plugin, and SHALL be
editable, replaceable and removable on the same terms as every other bundled
plugin.

Removing or replacing files SHALL be requested as a command rather than
performed by the plugin, since plugins have no filesystem access.

#### Scenario: The inventory surface is removed

- **WHEN** a user deletes the inventory plugin
- **THEN** it is removed like any other plugin, and no other behaviour depends on its presence

### Requirement: An empty interface is a choice, not a fault

Removing every plugin SHALL be a valid outcome and SHALL NOT be treated as a
broken plugin directory. The system SHALL NOT deliver the bundled plugins again
in order to fill an interface the user deliberately emptied.

Kernel-owned surfaces SHALL remain reachable in that state, so that an interface
with no plugins can still be repaired from inside the application.

#### Scenario: Every plugin is removed

- **WHEN** a user removes every bundled plugin and restarts
- **THEN** no pane renders, the removals stand, and the reserved keys and kernel-owned surfaces still respond

#### Scenario: The recovery floor is distinguished from an emptied interface

- **WHEN** the plugin directory cannot be loaded at all
- **THEN** the embedded copies render and the fallback is reported, which SHALL NOT happen merely because every plugin was deliberately removed
