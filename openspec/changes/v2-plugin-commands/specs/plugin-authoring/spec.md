# plugin-authoring Specification

## ADDED Requirements

### Requirement: A plugin may be written as several modules

A plugin SHALL be able to span several files inside the interface directory,
loading its own modules by name. A module that is not part of the interface
directory SHALL NOT be loadable. This holds for a plugin the user wrote, not
only for the modules that ship with thurbox.

#### Scenario: A plugin split across files

- **WHEN** a plugin loads a module of its own from inside the interface directory
- **THEN** the module loads, and the plugin renders

#### Scenario: A module outside the interface directory

- **WHEN** a plugin tries to load a module by a path that escapes the interface
  directory
- **THEN** it is refused with a reason naming what was asked for

#### Scenario: The check tool sees the whole plugin

- **WHEN** a multi-module plugin is checked without launching the interface
- **THEN** a failure in any of its modules is reported, naming the file

### Requirement: A capability a plugin needs is declared and checkable

A plugin SHALL declare the capabilities it needs. The check tool SHALL report a
declaration the interface would refuse — an unknown capability, or one the plugin
has not been trusted with — before the plugin is ever loaded in a running
interface.

#### Scenario: An unknown capability is declared

- **WHEN** a plugin declares a capability the interface does not offer
- **THEN** the check reports it and exits non-zero

#### Scenario: A declared capability has not been granted

- **WHEN** a plugin declares a capability the user has not trusted it with
- **THEN** the check reports that the plugin will load but be refused that
  capability, and does not treat it as a load failure

#### Scenario: The starter needs no capability

- **WHEN** a new plugin is created by the tooling
- **THEN** it loads and draws without declaring any capability

### Requirement: A worked example proves a composite pane is writable

The documentation SHALL carry a worked example of a pane built over more than
one third-party program, parsing their output rather than echoing it, and
rendering a widget composed from the published primitives. The example SHALL
load and draw as shipped.

#### Scenario: The example is checked

- **WHEN** the shipped example is checked the way the interface loads it
- **THEN** it loads without error

#### Scenario: The example is a genuine composite

- **WHEN** the example is read
- **THEN** it uses more than one program, derives values from their output, and
  builds its widget from the primitives rather than from a new node kind
