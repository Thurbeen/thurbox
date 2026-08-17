# plugin-switching Specification

## Purpose
Defines how a file of the interface is turned off and on again without being
deleted, and how a removal that can be undone is told apart from one that cannot
— so that wanting a pane gone for the afternoon never costs a user the plugin
they wrote.

## ADDED Requirements

### Requirement: A plugin can be turned off without being deleted

The system SHALL support a plugin being present on disk and not loaded. Turning
one off SHALL NOT modify, move or delete its file, and SHALL be reversible by the
same means that turned it off.

#### Scenario: A plugin is turned off

- **WHEN** a user turns a plugin off
- **THEN** its file is unchanged on disk
- **AND** the plugin is not loaded

#### Scenario: A plugin is turned back on

- **WHEN** a user turns a previously disabled plugin back on
- **THEN** it loads and draws again
- **AND** nothing of it had to be restored, because nothing was lost

#### Scenario: The choice outlives the session

- **WHEN** the interface is started again
- **THEN** a plugin turned off previously is still off

### Requirement: Turning a plugin off takes effect without a restart

A change to whether a plugin is loaded SHALL be visible without restarting the
interface.

#### Scenario: Turning one off

- **WHEN** a plugin that is on screen is turned off
- **THEN** it stops being drawn, without the interface being restarted

#### Scenario: Turning one on

- **WHEN** a disabled plugin is turned on
- **THEN** it appears, without the interface being restarted

### Requirement: A disabled plugin is inert

A disabled plugin SHALL contribute nothing: no keys, no settings, no action-band
entries, no slot occupancy, and no granted capability. Its state SHALL be
indistinguishable, to the rest of the interface, from the file not being there.

#### Scenario: Its keys are free

- **WHEN** a plugin declaring a key is disabled
- **THEN** that key is unbound, and may be claimed by another plugin without a
  conflict being reported

#### Scenario: Its settings are gone

- **WHEN** a plugin declaring a setting is disabled
- **THEN** that setting is not offered

#### Scenario: Its slot is released

- **WHEN** a disabled plugin declared a slot
- **THEN** the slot is filled by whatever else occupies it, or is placed and empty

#### Scenario: Its capabilities are not granted

- **WHEN** a plugin that declared a capability and was trusted is disabled
- **THEN** it is granted nothing, because it is not running

#### Scenario: A failure in it cannot break the interface

- **WHEN** a plugin that would fail to load is disabled
- **THEN** the interface loads without that failure

### Requirement: Disabled is reported as its own state

The inventory of the interface SHALL report a disabled file as disabled,
distinctly from a file that was removed, one whose slot is not placed, and one
that failed to load. The report SHALL be available both inside the running
interface and to a caller with no terminal.

#### Scenario: The inventory is read inside the interface

- **WHEN** the interface's own file list is shown
- **THEN** a disabled file is identifiable as disabled rather than as missing or
  merely not drawn

#### Scenario: The inventory is read without a terminal

- **WHEN** the interface's files are listed from a script
- **THEN** the disabled ones are reported as such

### Requirement: A removal that cannot be undone says so before it happens

Before a file is deleted, the system SHALL state whether it can be restored
afterwards. A file the system ships SHALL be identified as restorable; a file the
user provided SHALL be identified as one the system holds no copy of, and whose
deletion is permanent.

#### Scenario: Removing a shipped file

- **WHEN** a user is asked to confirm removing a file the system ships
- **THEN** the request says it can be restored afterwards

#### Scenario: Removing the user's own file

- **WHEN** a user is asked to confirm removing a file the system does not ship
- **THEN** the request says the system holds no copy and the removal cannot be
  undone

#### Scenario: The two are not worded alike

- **WHEN** the two confirmations are compared
- **THEN** they differ in what they say about recovery, so a reflex learned on one
  does not carry to the other

### Requirement: Turning a plugin off is easier to reach than deleting it

The reversible action SHALL be at least as easy to reach as the destructive one,
and the two SHALL NOT be reachable by the same keystroke. Deleting SHALL require
a confirmation; turning off SHALL NOT.

#### Scenario: Turning off is immediate

- **WHEN** a user turns a plugin off
- **THEN** it happens without a confirmation, because nothing is at risk

#### Scenario: Deleting is confirmed

- **WHEN** a user asks to delete a file
- **THEN** it happens only after a confirmation naming the file

#### Scenario: A slip does not destroy anything

- **WHEN** the key for turning a plugin off is pressed by mistake
- **THEN** no file is deleted, and the action is undone by pressing it again

### Requirement: Adding a plugin is discoverable from inside the interface

The surface that lists the interface's files SHALL say where those files live and
that adding one is a matter of putting a file there.

#### Scenario: A user looks for how to add a pane

- **WHEN** the interface's file list is shown
- **THEN** the directory the files are read from is named, and adding one is
  described as adding a file to it

#### Scenario: An empty directory

- **WHEN** the interface has no plugin files of the user's own
- **THEN** the list still says where they would go
