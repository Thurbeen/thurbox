# core-settings Specification

## Purpose
Defines what the user's configuration file controls in the interface — which
features exist, how notifications behave, how much scrollback is kept, when a
column appears, whether updates are checked for and installed — and the contract
around changing it: what takes effect at once, what waits for the next launch,
and how the difference is made visible rather than guessed at.
## Requirements
### Requirement: A disabled feature is absent, not merely hidden

Each feature switch the system honours SHALL remove the feature: its surface is
not drawn, and the key that would reach it does nothing. Data and headless
surfaces SHALL remain fully functional, so turning a switch back on loses
nothing.

#### Scenario: A feature is switched off

- **WHEN** a feature the interface honours is disabled in configuration
- **THEN** its surface is not shown and its key does nothing
- **AND** nothing about the underlying data is changed or lost

#### Scenario: It is switched back on

- **WHEN** the same feature is re-enabled
- **THEN** its surface returns with the state it would have had

#### Scenario: A switch the interface has no surface for

- **WHEN** configuration carries a switch for a feature this interface does not
  implement
- **THEN** it is preserved in the file untouched and never presented as though it
  did something

### Requirement: Deleting a session follows the soft-delete switch

With soft delete enabled, deleting a session in the interface SHALL mark it
deleted and keep its pane and worktrees, offering an undo. With it disabled, the
interface SHALL delete for real — tearing down the pane and the worktrees — and
SHALL first confirm, itemising what would be lost, because there is no undo for
it.

#### Scenario: Soft delete is enabled

- **WHEN** a session is deleted
- **THEN** it disappears from the list, its pane and worktrees survive, and the
  deletion can be undone

#### Scenario: Soft delete is disabled

- **WHEN** a session is deleted
- **THEN** a confirmation names what would be lost before anything is torn down
- **AND** confirming removes the pane and the worktrees with it

#### Scenario: The headless surface is unaffected

- **WHEN** a session is deleted from the command line
- **THEN** the switch changes nothing: it stays a soft delete unless force is asked for

### Requirement: Notification behaviour follows every notification setting

The system SHALL honour each notification setting it exposes: which transition
notifies, whether the focused session is skipped, whether a sound plays, the
minimum interval between two notifications for one session, and which delivery
backend is used. A setting that is exposed and not honoured is a defect.

#### Scenario: Only the blocked edge notifies by default

- **WHEN** a session finishes its turn and the finish-notification setting is off
- **THEN** nothing is delivered
- **AND** a session becoming blocked still delivers

#### Scenario: The finish edge is enabled

- **WHEN** the finish-notification setting is on and a session finishes
- **THEN** a notification is delivered

#### Scenario: Two transitions in quick succession

- **WHEN** one session transitions twice within the configured minimum interval
- **THEN** only the first is delivered

#### Scenario: Delivery is switched off

- **WHEN** the backend setting selects no delivery
- **THEN** nothing is delivered, and the feature switch is left as it is

### Requirement: Scrollback, layout thresholds and retention follow configuration

The system SHALL keep the configured number of scrollback lines per session
terminal, SHALL use the configured widths as the thresholds at which the second
and third columns become available, and SHALL prune audit history to the
configured retention at startup.

#### Scenario: A wider column threshold is configured

- **WHEN** the second-column threshold is raised above the terminal's width
- **THEN** only the central pane is drawn

#### Scenario: Scrollback is raised

- **WHEN** more scrollback lines are configured than the default
- **THEN** a session's terminal keeps that many lines of history

#### Scenario: Retention is configured

- **WHEN** the interface starts
- **THEN** audit history older than the configured retention is no longer kept

### Requirement: Updates are checked for and installed only when asked for

With the update check enabled, the system SHALL report that a newer release
exists, refreshing what it knows in the background without blocking the
interface. With silent updating enabled, it SHALL install the newer release on
startup and report that a restart will apply it. With either disabled, the system
SHALL make no network call for it and SHALL replace nothing on disk.

#### Scenario: A newer release exists and the check is enabled

- **WHEN** the interface starts and a newer release is known
- **THEN** it is reported in the interface
- **AND** learning this never blocks drawing or input

#### Scenario: The check is disabled

- **WHEN** the check is disabled
- **THEN** nothing is reported and no network call is made for it

#### Scenario: Silent updating is enabled

- **WHEN** silent updating is enabled and a newer release exists
- **THEN** it is installed and the interface says a restart will apply it

#### Scenario: Silent updating is disabled

- **WHEN** silent updating is disabled
- **THEN** nothing on disk is replaced, whatever the check reports

#### Scenario: A development build

- **WHEN** the running build is not a release
- **THEN** nothing is installed, because there is no released version to compare against

### Requirement: A configuration change takes effect without a restart where it can

The system SHALL notice a change to the configuration file while running and
apply it. Settings that cannot take effect until the next launch SHALL be applied
at the next launch and SHALL be reported as such rather than appearing to have
been applied.

#### Scenario: A live setting changes on disk

- **WHEN** the file is edited outside the interface and a live setting changes
- **THEN** the change takes effect without a restart

#### Scenario: A restart-only setting changes on disk

- **WHEN** the file is edited and only a restart-only setting changes
- **THEN** the interface says a restart is needed rather than implying the change is active

#### Scenario: The file becomes invalid

- **WHEN** the file cannot be parsed
- **THEN** the settings in force are kept, and the problem is reported

### Requirement: Core settings are editable from inside the interface

The system SHALL let the settings it honours be viewed and changed from the
interface, alongside the settings plugins declare. A core change SHALL be written
back to the configuration file, preserving the file's existing comments, and a
row that cannot take effect until the next launch SHALL be marked as such
*before* it is changed.

#### Scenario: A core setting is changed and saved

- **WHEN** a core setting is edited and the change is saved
- **THEN** the configuration file carries the new value, with its comments intact
- **AND** a live setting takes effect at once

#### Scenario: A core edit is abandoned

- **WHEN** core settings are edited and the modal is dismissed without saving
- **THEN** nothing is written and nothing changes

#### Scenario: A restart-only setting is changed

- **WHEN** a restart-only setting is saved
- **THEN** the interface says the change applies at the next launch

#### Scenario: A plugin setting is changed in the same view

- **WHEN** a plugin's setting is changed
- **THEN** it takes effect immediately, as it does today, and the configuration
  file is not involved

### Requirement: The effective settings are readable by a plugin

The system SHALL publish the settings in force, so a pane can honour a setting
the kernel has no knowledge of — including a feature switch for a surface the
kernel does not own.

#### Scenario: A pane reads a switch

- **WHEN** a plugin reads the published settings
- **THEN** it sees the values in force, including any change applied since startup

#### Scenario: A pane honours its own switch

- **WHEN** a plugin's own feature switch is disabled
- **THEN** the plugin can tell, and can decline to draw

