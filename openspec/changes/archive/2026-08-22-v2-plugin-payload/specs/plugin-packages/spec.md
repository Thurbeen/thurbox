## ADDED Requirements

### Requirement: A plugin that carries more than Lua is acquired as a repository

The system SHALL install a plugin from a **version-control source** by obtaining a
working copy of it, and everything that source contains SHALL be delivered — code
that is not Lua, binary programs, data files — not only the files a manifest
enumerates.

The source form SHALL select this: a source recognisable as a repository is
obtained this way, and every other form keeps the behaviour it already has. A
plugin author therefore chooses the mechanism by choosing how to publish, and
nothing that installs today changes.

The working copy SHALL be placed **within the interface directory**, under a
directory of the plugin's own, so that everything the interface is made of remains
in one tree.

#### Scenario: Installing from a repository

- **WHEN** a plugin is installed from a version-control source
- **THEN** a working copy is placed under the interface directory in a directory of
  that plugin's own
- **AND** files that are not Lua are delivered along with those that are
- **AND** the spec records the source and the destination

#### Scenario: The layout is the author's

- **WHEN** the source arranges its files in directories of its own choosing
- **THEN** that arrangement is preserved rather than flattened or rearranged

#### Scenario: Installing from a non-repository source

- **WHEN** a plugin is installed from a source that is not a repository
- **THEN** it is delivered exactly as it is today, and only Lua is accepted

#### Scenario: The destination is already occupied

- **WHEN** a working copy would be placed where something unmanaged already exists
- **THEN** the install is refused, naming what is there
- **AND** the existing files are left exactly as they were

### Requirement: What was obtained is recorded precisely enough to reproduce

The system SHALL record, for a plugin acquired as a repository, the exact revision
obtained — not a name that could later point somewhere else. Applying the same spec
and record elsewhere SHALL obtain that same revision.

This replaces a per-file integrity list for such a plugin: the revision identifies
every byte delivered, cannot be maintained inconsistently with the payload, and is
produced by the source rather than transcribed by its author.

#### Scenario: An install is recorded

- **WHEN** a plugin is installed from a repository
- **THEN** the exact revision obtained is recorded, not only the name asked for

#### Scenario: The record is applied elsewhere

- **WHEN** the same spec and record are applied where nothing is installed
- **THEN** the recorded revision is obtained, not whatever the name now points at

### Requirement: A pane may live outside the panes directory

The system SHALL load a plugin the spec names even when it does not sit in the
directory panes are otherwise found in, because a repository arranges its own files.
Its position in the load order SHALL be determined the same way it is for any other
pane, so ordering does not depend on where a plugin came from.

#### Scenario: A pane inside a working copy

- **WHEN** the spec names a pane inside an installed plugin's own directory
- **THEN** that pane is loaded

#### Scenario: Its place in the order

- **WHEN** such a pane is loaded alongside panes in the usual directory
- **THEN** it takes its place in the load order by the same rule as the others

#### Scenario: A file the spec does not name

- **WHEN** an installed plugin's directory holds Lua the spec does not name as a
  pane
- **THEN** it is not loaded as one, and remains available to plugins that require it

### Requirement: Convergence and advancing defer to the source's own working copy

For a plugin acquired as a repository, the system SHALL let that source's own rules
govern the working copy rather than applying the delivery reconciliation used for
enumerated files. Specifically:

- Local modifications SHALL NOT be discarded. An operation that would overwrite them
  SHALL be refused or reported, never silently completed.
- Advancing to a newer revision SHALL happen only when asked, and SHALL report what
  moved and from what.
- Converging an already-current working copy SHALL change nothing and report success.

#### Scenario: The working copy has local modifications

- **WHEN** convergence or advancing would overwrite a modified file in a working copy
- **THEN** the modification is preserved
- **AND** the outcome says so rather than reporting a clean result

#### Scenario: Already at the recorded revision

- **WHEN** convergence runs against a working copy already at the recorded revision
- **THEN** nothing is changed and success is reported

#### Scenario: Advancing a repository plugin

- **WHEN** such a plugin is advanced
- **THEN** the newer revision is obtained and the change is reported with the
  revision it came from

#### Scenario: Removing a repository plugin

- **WHEN** a plugin acquired as a repository is removed
- **THEN** its working copy, its spec entry and its record are gone
- **AND** the removal does not require the source to still be reachable

### Requirement: A plugin can tell what machine it is running on

The system SHALL publish the operating system and processor architecture to plugins,
so that a plugin delivering more than one build can choose between them itself.

Selecting a build SHALL NOT be expressed in a package manifest. The plugin knows
things the kernel cannot model — which of several builds suits this machine, whether
to prefer something already installed, whether to fall back to a portable build —
and a plugin that finds nothing suitable SHALL be able to say so in its own pane.

#### Scenario: A plugin reads the platform

- **WHEN** a plugin renders
- **THEN** it can read the operating system and architecture it is running on

#### Scenario: A plugin finds nothing for this machine

- **WHEN** a plugin delivers no build suitable for the running platform
- **THEN** it can report that in its own pane rather than the install having failed

### Requirement: Installing delivers files; running still requires the grant

The system SHALL deliver a repository's files as that repository contains them,
including any that are marked executable, and SHALL NOT thereby permit anything to
run. Running a program remains gated on the capability the user grants per file.

This SHALL be stated plainly wherever installing is documented: installing a plugin
from a repository places that repository's files on the user's disk.

#### Scenario: A delivered executable does not run

- **WHEN** a plugin is installed whose source contains an executable file
- **THEN** the file is delivered
- **AND** nothing is executed as part of installing
- **AND** the plugin still may not run it until the user grants it that capability

#### Scenario: Nothing is executed on the user's behalf at install time

- **WHEN** a plugin is installed
- **THEN** no script or command from the source is run

### Requirement: Version-control bookkeeping is not an interface change

The system SHALL NOT treat a version-control directory inside the interface
directory as a change to the interface. Reloading is driven by edits to the
interface's own files, and bookkeeping written by version control SHALL NOT trigger
one.

This holds whether that directory arrived with an installed plugin or because the
user placed the interface under version control themselves, which is a reasonable
thing to do and must not make the interface reload continuously.

#### Scenario: Version-control bookkeeping changes

- **WHEN** files inside a version-control directory under the interface change
- **THEN** the interface is not reloaded

#### Scenario: An interface file changes

- **WHEN** a plugin, module or the arrangement changes
- **THEN** the interface is reloaded as before
