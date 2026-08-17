## Purpose

Defines which plugins ship with thurbox and how they are delivered, so that a
fresh install has a working interface, a user can read and edit that interface's
source, and a broken edit can never leave the application with nothing to draw.

## ADDED Requirements

### Requirement: Bundled plugins ship inside the binary

The plugins that constitute the default interface SHALL be embedded in the
thurbox binary. A working interface SHALL NOT depend on any file installed
alongside the binary.

#### Scenario: A binary is run with no supporting files

- **WHEN** the binary is run with no plugin directory present anywhere
- **THEN** the default interface renders

### Requirement: Bundled plugins are written to disk on first run

On first run the system SHALL write the embedded plugins into the user's plugin
directory, so that the shipped interface is readable and editable as ordinary
files.

The system SHALL NOT overwrite a file the user has modified.

#### Scenario: First run

- **WHEN** thurbox runs for the first time
- **THEN** the embedded plugins are written to the user's plugin directory

#### Scenario: An upgrade follows a user edit

- **WHEN** a newer binary runs and a bundled plugin has been modified by the user
- **THEN** the user's version is preserved and the difference is surfaced

#### Scenario: An unmodified bundled plugin is superseded

- **WHEN** a newer binary runs and a bundled plugin has not been modified
- **THEN** it is updated to the version the binary carries

### Requirement: A user copy takes precedence over the embedded copy

Where a plugin exists both on disk and embedded in the binary, the copy on disk
SHALL be used.

#### Scenario: A bundled plugin is edited

- **WHEN** a user edits a bundled plugin on disk
- **THEN** the edited version is loaded in preference to the embedded one

### Requirement: The embedded copies are the recovery floor

When the plugin directory is missing, or when loading from it fails, the system
SHALL fall back to the embedded plugins and render a working interface. The
system SHALL tell the user that it has fallen back and why.

The application SHALL NOT reach a state in which no interface is rendered
because of a plugin fault.

#### Scenario: The plugin directory is deleted

- **WHEN** the user's plugin directory is deleted and thurbox is started
- **THEN** the embedded plugins render and the fallback is reported

#### Scenario: A user edit breaks the whole environment

- **WHEN** an edit prevents the plugin environment from loading at startup
- **THEN** the embedded plugins render, the failure is reported with its file, and the user's files are left untouched

#### Scenario: The environment is repaired

- **WHEN** the user corrects the fault and a reload occurs
- **THEN** the user's plugins are loaded again in preference to the embedded ones

### Requirement: The bare core is the default interface

The bundled set SHALL be limited to what is required to operate sessions: a
session list, a central agent view, an arrangement, a theme, a status line, a
help surface, a settings surface, a session-creation flow and a destructive-action
confirmation.

No pane SHALL be implemented in the kernel. In particular the session list and
the central agent view SHALL be plugins, replaceable by the user on the same
terms as any other plugin.

#### Scenario: A user replaces the session list

- **WHEN** a user replaces the bundled session list with their own
- **THEN** it is loaded in preference to the bundled one and no kernel behaviour depends on the bundled version

#### Scenario: The default interface is inspected

- **WHEN** a user inspects the shipped interface
- **THEN** every pane is present as a readable plugin file

### Requirement: The default interface reproduces v1's arrangement

The bundled arrangement SHALL reproduce thurbox v1's screen: a session list
beside a central agent view, with the size thresholds at which regions appear
and disappear preserved. The central region SHALL be a switched slot, so that
further views can be contributed to it by later plugins.

#### Scenario: A v1 user starts v2

- **WHEN** a user familiar with v1 starts v2
- **THEN** the session list and central agent view occupy the same positions and respond to the same size thresholds

#### Scenario: A plugin contributes a central view

- **WHEN** a later plugin declares the central slot
- **THEN** it becomes selectable alongside the agent view with no change to the bundled plugins

### Requirement: Bundled plugins hold no privileged capability

A bundled plugin SHALL run with the same capabilities as any other plugin, and
SHALL NOT rely on any binding unavailable to a user-written one.

#### Scenario: A bundled plugin is reimplemented by a user

- **WHEN** a user writes their own replacement for a bundled plugin
- **THEN** every capability the bundled version used is available to theirs
