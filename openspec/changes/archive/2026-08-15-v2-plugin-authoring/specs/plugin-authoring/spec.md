## Purpose

Defines what someone must be able to do to write a plugin without launching the
interface: find the directory that is actually being read, start from something
that already works, and be told whether what they wrote will load — each of them
answerable from a script, a pipe, or a session with no terminal to press keys in.

## ADDED Requirements

### Requirement: The directory in force is answerable without launching the interface

The interface chooses its plugin directory by a documented order. The system
SHALL report the directory it would use, and which rule selected it, without
starting the interface and without a terminal.

#### Scenario: The directory is asked for

- **WHEN** the plugin directory is asked for
- **THEN** the absolute path is reported, along with the rule that chose it

#### Scenario: An override is set

- **WHEN** the environment names a directory explicitly
- **THEN** that directory is reported, and the report says the override chose it

#### Scenario: The answer is consumed by a script

- **WHEN** the report is read by a program rather than a person
- **THEN** it is machine-readable, and the path is available on its own

#### Scenario: The interface and the report disagree

- **WHEN** the interface loads its plugins
- **THEN** it uses the same directory the report names, by construction rather
  than by two implementations agreeing

### Requirement: A new plugin starts from something that already works

The system SHALL create a starter plugin in the directory in force, valid on its
first load, and SHALL refuse to overwrite an existing file.

#### Scenario: A plugin is created

- **WHEN** a new plugin is asked for by name
- **THEN** a file is written into the directory in force and its path is reported
- **AND** loading the interface afterwards reports no error for it

#### Scenario: The name is already taken

- **WHEN** the named file already exists
- **THEN** nothing is written and the refusal names the existing file

#### Scenario: The name is not usable as a file

- **WHEN** the name contains path separators or would escape the directory
- **THEN** it is refused rather than sanitised into something else

### Requirement: What was written can be verified without a terminal

The system SHALL load the interface exactly as the running interface does and
report the outcome per file: which plugins loaded, and for each that did not, why.
The result SHALL be usable as a gate — success and failure distinguishable by
exit status.

#### Scenario: Every plugin loads

- **WHEN** the interface is checked and every file is valid
- **THEN** it reports success, and the exit status says so

#### Scenario: One plugin is broken

- **WHEN** a plugin fails to load
- **THEN** the failure is reported with the file and the reason
- **AND** the exit status distinguishes this from success
- **AND** the plugins that did load are still reported as loaded

#### Scenario: The directory has no plugins at all

- **WHEN** the interface is checked with every plugin removed
- **THEN** that is reported as an interface with no panes, not as a failure

### Requirement: What is loaded is listable without a terminal

The system SHALL report every file of the interface, where it came from —
shipped, edited, the user's own, or removed — and whether it is currently on
screen, which is what the interface's own inventory view shows.

#### Scenario: The interface is listed

- **WHEN** the files of the interface are listed
- **THEN** each is reported with its origin and whether it is on screen

#### Scenario: A shipped file was edited

- **WHEN** a shipped file has been modified
- **THEN** it is reported as edited rather than as shipped

### Requirement: The starter and the documented example are the same text

The example shown in the written guide and the file a new plugin starts from
SHALL be one artifact, so neither can drift from the other, and it SHALL be
proved to load rather than asserted to.

#### Scenario: The example is loaded

- **WHEN** the interface is built from the example alone
- **THEN** it loads with no error and renders

#### Scenario: The example changes

- **WHEN** the example is edited
- **THEN** both the guide and what a new plugin starts from change with it

### Requirement: The written guide leads with what is needed first

The guide SHALL open with the path from nothing to a working plugin — where the
file goes, the smallest thing that works, how to see it, how to check it — before
its reference material. It SHALL record the mistakes that are invisible until
runtime, including that reading persistent state yields a copy that must be
written back.

#### Scenario: A first plugin is written from the guide

- **WHEN** a reader follows the opening section
- **THEN** they have a loaded, visible plugin without reading the reference

#### Scenario: A trap is hit

- **WHEN** a plugin mutates persistent state without writing it back
- **THEN** the guide names that failure, because nothing else does — it is silent
  at load, at lint, and at render
