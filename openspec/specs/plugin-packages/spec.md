# plugin-packages Specification

## Purpose
Defines how an interface stops being a directory somebody copied files into and
becomes something written down: what it is composed of, how a pane is acquired
without a terminal, how the directory is converged to that composition and back
out again, and what is recorded so the same spec produces the same interface
twice — while an edit the user made to an installed pane is still theirs, and a
grant made to one version does not silently carry to the next.
## Requirements
### Requirement: What the interface is made of is written down

The system SHALL read a declarative spec in the interface directory listing the
plugins that interface is composed of. Each entry SHALL name a source, the file it
is delivered to, and optionally a pin identifying the version to use. The spec
SHALL be a hand-editable text format, so that a person or an agent may change it
with an ordinary edit and a malformed change is reported as a parse failure naming
its location rather than surfacing later as a missing pane.

The spec SHALL be authoritative over what is installed, and absent from it SHALL
mean not installed — so that the interface can be reproduced from it.

#### Scenario: The spec lists an entry

- **WHEN** the spec names a source and a destination file
- **THEN** that entry is reported as part of the interface, whether or not the
  file is present yet

#### Scenario: The spec is malformed

- **WHEN** the spec cannot be parsed
- **THEN** the failure names the spec and the location of the problem
- **AND** no plugin is installed, removed or modified as a result
- **AND** the exit status distinguishes this from success

#### Scenario: There is no spec

- **WHEN** the interface directory has no spec
- **THEN** that is reported as an interface with nothing installed, not as a
  failure, and the shipped plugins are unaffected

### Requirement: A plugin can be acquired without a terminal

The system SHALL install a plugin from a source into the interface directory
without requiring an interactive terminal, recording the entry in the spec. A bare
name SHALL resolve against the officially distributed set for the running release;
a URL or a filesystem path SHALL resolve to that location. Resolution SHALL follow
the same rules the system already applies to extension sources, so that one
vocabulary covers both.

#### Scenario: Installing by bare name

- **WHEN** a plugin is installed by a name alone
- **THEN** it resolves against the official set for the running release
- **AND** the plugin is written into the interface directory
- **AND** the spec records the source, the destination and the resolved version

#### Scenario: Installing from a URL or a path

- **WHEN** a plugin is installed from a URL or a filesystem path
- **THEN** it is written into the interface directory from that location
- **AND** the spec records the source it came from

#### Scenario: A bare name that does not exist

- **WHEN** a plugin is installed by a name the official set does not contain
- **THEN** the failure says the name was not found and names the alternatives
- **AND** nothing is written into the interface directory

#### Scenario: The destination is already occupied by an unmanaged file

- **WHEN** a plugin would be written over a file the spec does not manage
- **THEN** the install is refused, naming the file
- **AND** the existing file is left exactly as it was

### Requirement: The directory can be converged to the spec

The system SHALL bring the interface directory into agreement with the spec in one
operation: installing entries that are absent, removing files it previously
installed that the spec no longer lists, and leaving everything else alone. The
outcome SHALL be reported per entry, and the exit status SHALL distinguish a
converged directory from one that could not be converged.

Convergence SHALL be repeatable: applying it to an already-converged directory
SHALL change nothing and SHALL report success.

#### Scenario: An entry is missing from the directory

- **WHEN** the spec lists an entry whose file is absent
- **THEN** convergence installs it and reports it as installed

#### Scenario: An entry was removed from the spec

- **WHEN** a file the system installed is no longer listed in the spec
- **THEN** convergence removes it and reports it as removed

#### Scenario: The directory already agrees with the spec

- **WHEN** convergence runs against a directory that already matches
- **THEN** nothing is changed and success is reported

#### Scenario: A file the system did not install

- **WHEN** the directory holds a plugin the spec never listed
- **THEN** convergence leaves it untouched and does not report it as a problem

### Requirement: An edit to an installed plugin is not silently destroyed

The system SHALL treat a change the user made to an installed plugin as theirs to
keep. An install or convergence that would overwrite a modified file SHALL preserve
the modification and report that it did so, rather than replacing it. Advancing to
a new version SHALL require the user to say so.

#### Scenario: An installed plugin was edited

- **WHEN** convergence would rewrite a file the user has modified
- **THEN** the modification is preserved
- **AND** the outcome reports that this file was kept rather than updated

#### Scenario: An installed plugin was deleted

- **WHEN** a file the system installed has been deleted by the user
- **THEN** the deletion is remembered, and convergence does not silently
  reinstall it

### Requirement: The same spec produces the same interface

The system SHALL record what each entry resolved to, so that applying the same
spec elsewhere delivers the same plugins. The record SHALL be a machine-written
file in the interface directory, distinct from the hand-edited spec.

#### Scenario: An entry is installed

- **WHEN** an entry is installed or updated
- **THEN** what it resolved to is recorded

#### Scenario: The spec is applied on another machine

- **WHEN** the same spec and record are applied where nothing is installed
- **THEN** each entry resolves to what the record names, not to whatever is
  newest

#### Scenario: The record disagrees with the spec

- **WHEN** the record names an entry the spec no longer lists
- **THEN** the record is brought back into agreement as part of convergence

### Requirement: A pin can be moved forward deliberately

The system SHALL advance an entry to a newer version only when asked, and SHALL
report what moved and from what. Advancing SHALL be possible for one entry or for
all of them.

#### Scenario: Updating one entry

- **WHEN** one entry is updated
- **THEN** its pin and record are advanced, and the change is reported with the
  version it came from

#### Scenario: Updating everything

- **WHEN** every entry is updated at once
- **THEN** each entry that moved is reported, and the ones already current are
  reported as unchanged

#### Scenario: Nothing has changed upstream

- **WHEN** an update finds no newer version
- **THEN** that is reported as already current, not as a failure

### Requirement: An installed plugin can be removed

The system SHALL remove a plugin it installed, delete its entry from the spec and
its record, and report what was removed. Removal SHALL not require the source to
still be reachable.

#### Scenario: Removing an installed plugin

- **WHEN** an installed plugin is removed
- **THEN** its file, its spec entry and its record are gone
- **AND** the removal is reported

#### Scenario: Removing something that was never installed

- **WHEN** a removal names a plugin the spec does not list
- **THEN** the failure says so and nothing is changed

### Requirement: Trusting an installed plugin survives its updates

For a plugin the system installed, the user's decision to grant it a capability
SHALL be recorded against the source and version it was granted for, rather than
against the file's contents. An update within that same source and version SHALL
NOT present itself as a modification the user did not make. A first install SHALL
still require the grant, and changing the pin SHALL require it again.

This exists because a capability grant is a statement about provenance — "I trust
this plugin, at this version" — and a statement about file contents cannot express
it: every legitimate update changes the contents, so a manager keyed on contents
reports every release as tampering and teaches the user to dismiss the warning.

#### Scenario: A capability is granted to an installed plugin

- **WHEN** the user grants a capability to a plugin the system installed
- **THEN** the grant is recorded against its source and version

#### Scenario: The plugin is reinstalled at the same version

- **WHEN** the same source and version is installed again
- **THEN** the grant still applies
- **AND** the plugin is not reported as modified

#### Scenario: The pin is moved

- **WHEN** an installed plugin is advanced to a different version
- **THEN** the capability is not granted to the new version until the user says so

#### Scenario: The user edits an installed plugin that holds a grant

- **WHEN** a plugin with a granted capability is modified locally
- **THEN** it is reported as modified, because the contents no longer come from
  the source the grant was made against

### Requirement: A third-party module has somewhere to live

The system SHALL allow a plugin delivered from a source to bring shared modules
without colliding with the shipped ones or with another source's. A module
delivered this way SHALL be requirable by the plugin that came with it.

#### Scenario: Two sources ship a module of the same name

- **WHEN** two installed plugins each bring a module with the same file name
- **THEN** both are delivered, and each plugin requires its own

#### Scenario: A source ships a module named like a shipped one

- **WHEN** an installed plugin brings a module whose name matches a shipped module
- **THEN** the shipped module is not replaced
- **AND** plugins requiring the shipped module continue to get it

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
