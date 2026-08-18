## ADDED Requirements

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
