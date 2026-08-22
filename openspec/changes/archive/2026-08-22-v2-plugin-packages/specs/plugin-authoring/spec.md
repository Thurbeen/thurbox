## MODIFIED Requirements

### Requirement: What was written can be verified without a terminal

The system SHALL load the interface exactly as the running interface does and
report the outcome per file: which plugins loaded, and for each that did not, why.
It SHALL additionally report a plugin that loaded but which the arrangement places
nowhere, because such a plugin is indistinguishable from a working one by every
other signal — it loads without error, declares its keys, and draws nothing. For
that case the report SHALL name the slot that is unplaced and state what to add to
the arrangement. The result SHALL be usable as a gate — success and failure
distinguishable by exit status.

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

#### Scenario: A plugin loaded but nothing places it

- **WHEN** a plugin loads and no arrangement places the slot it occupies
- **THEN** it is reported as loaded but unplaced, naming the file and the slot
- **AND** the report states what to add to the arrangement to place it
- **AND** the exit status distinguishes this from success

#### Scenario: A plugin that draws above the arrangement

- **WHEN** a plugin that floats rather than occupying a slot is checked
- **THEN** it is not reported as unplaced, because it needs no slot

#### Scenario: A plugin the user turned off

- **WHEN** a plugin is disabled and therefore not loaded
- **THEN** it is not reported as unplaced, because nothing was asked to place it
- **AND** it is not reported as loaded either

#### Scenario: A pane behind a closed column

- **WHEN** a plugin occupies a column the arrangement only places while that
  column is open, and the column starts closed
- **THEN** it is not reported as unplaced, because the arrangement does name its
  slot — it is the column's state, not the arrangement, that is withholding it

### Requirement: What is loaded is listable without a terminal

The system SHALL report every file of the interface, where it came from —
shipped, edited, the user's own, removed, or installed from a named source — and
whether it is currently on screen, which is what the interface's own inventory
view shows. For a file installed from a source, the report SHALL name that source,
so that a file the user did not write is distinguishable from one they did. This
matters because a capability is granted per file: who a file came from is the
question to answer before granting it.

#### Scenario: The interface is listed

- **WHEN** the files of the interface are listed
- **THEN** each is reported with its origin and whether it is on screen

#### Scenario: A shipped file was edited

- **WHEN** a shipped file has been modified
- **THEN** it is reported as edited rather than as shipped

#### Scenario: A file was installed from a source

- **WHEN** a plugin installed from a source is listed
- **THEN** it is reported as installed, naming the source
- **AND** it is not reported as the user's own

#### Scenario: An installed file was edited

- **WHEN** a plugin installed from a source has been modified locally
- **THEN** it is reported as installed and modified, naming the source
