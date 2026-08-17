## Purpose

Defines how a user chooses what a new session is made of — which host it runs
on, which repositories it spans, which of them get a worktree, the branch it is
cut from, its name and its agent — and what the system must expose so every one
of those choices can be offered, remembered and revised without the interface
ever waiting on a disk, a git fetch or an unreachable host.

## ADDED Requirements

### Requirement: The flow asks for a host only when there is a choice

The system SHALL offer the local machine plus every configured and discovered
host, each identified well enough to tell two apart (its transport and
destination, not only its name). When no host is configured, the flow SHALL
proceed against the local machine without asking.

#### Scenario: Hosts are configured

- **WHEN** the flow starts and one or more hosts are available
- **THEN** the choice includes the local machine alongside each host
- **AND** each host is shown with the detail that distinguishes it

#### Scenario: No hosts are configured

- **WHEN** the flow starts with no configured or discovered host
- **THEN** no host question is asked and the flow targets the local machine

### Requirement: Repository choices are remembered per host

The system SHALL remember the repositories a session was created against and
offer them again, most recently used first. That memory SHALL be scoped to the
host the repository lives on, so a remote target never offers paths from
another machine's filesystem.

#### Scenario: A remembered repository is offered again

- **WHEN** the repository step opens for a host
- **THEN** the repositories previously used on that host are listed, most recent first

#### Scenario: A remote target has its own memory

- **WHEN** the repository step opens for a remote host
- **THEN** no local path is offered, and no remote path is offered for a local target

#### Scenario: Nothing is remembered yet

- **WHEN** the repository step opens with no memory for that host
- **THEN** the step opens ready to accept a typed path

### Requirement: A session can be made of several repositories

The system SHALL allow more than one repository to be chosen for one session.
Each chosen repository SHALL independently be either **worktree mode** — taking
its own worktree on the session's shared branch — or attached **as-is**, and the
choice SHALL be visible per repository. A session with two or more members SHALL
launch its agent where every member is reachable.

#### Scenario: Two repositories are chosen, one in worktree mode

- **WHEN** two repositories are selected and only one is marked for a worktree
- **THEN** the created session has a worktree for that one and the other attached as it is
- **AND** the agent starts somewhere both are reachable

#### Scenario: Worktree mode is refused for a non-repository

- **WHEN** worktree mode is requested for a directory that is not a git repository
- **THEN** it is refused with the reason, and the directory stays selectable as a plain member

#### Scenario: Nothing is selected, locally

- **WHEN** the repository step is confirmed with nothing selected and the target is the local machine
- **THEN** a session is still created, in the user's home directory, with no worktree

#### Scenario: Nothing is selected, on a host

- **WHEN** the repository step is confirmed with nothing selected and the target is a host
- **THEN** a repository is asked for, because a local home directory is not a path on that machine

### Requirement: A repository can be chosen by typing its path

The system SHALL accept a repository path typed by the user, expanding a leading
`~` on the machine the session will run on. The path SHALL be checked to exist
**before** any branch or worktree work begins, and a path that does not exist
SHALL be refused with the typed text preserved so it can be corrected in place.
A newly accepted path SHALL be selected and remembered.

#### Scenario: An existing path is typed

- **WHEN** a path that exists is committed
- **THEN** it becomes a selected member and is remembered for that host

#### Scenario: A missing path is typed

- **WHEN** a path that does not exist is committed
- **THEN** it is refused with a message naming the path, nothing is remembered, and the text is still editable

#### Scenario: The same path is typed twice

- **WHEN** a path that is already listed is committed
- **THEN** it is selected rather than listed a second time

### Requirement: Typing a path is assisted by the filesystem it names

While a path is being typed, the system SHALL offer the sub-directories of the
directory component, filtered by what has been typed so far, and SHALL mark
which of them are git repositories. Selecting a plain directory SHALL descend
into it; selecting a repository SHALL choose it. The assistance SHALL come from
the machine the session will run on, and waiting for it SHALL never block the
interface.

#### Scenario: The listing is requested

- **WHEN** a directory's entries are asked for
- **THEN** its immediate sub-directories are offered, with the git ones marked

#### Scenario: The listing is slow or remote

- **WHEN** the entries have not arrived yet
- **THEN** the wait is visible, the rest of the interface keeps responding, and keystrokes are not lost

#### Scenario: The directory does not exist

- **WHEN** entries are asked for a directory that is not there
- **THEN** the failure is shown in place of the entries

### Requirement: A folder of repositories can be imported at once

The system SHALL accept a folder and offer the git repositories directly under
it as a named group, so a user with many repositories in one place need not add
them one at a time. A group SHALL be collapsible, its members individually
selectable, and the import SHALL report how many were found.

#### Scenario: A folder with repositories is imported

- **WHEN** a folder containing repositories is imported
- **THEN** it appears as a group whose members are the repositories under it
- **AND** the number found is reported

#### Scenario: A folder with no repositories is imported

- **WHEN** a folder containing no repository is imported
- **THEN** it is reported as empty rather than silently adding nothing

#### Scenario: A group is collapsed

- **WHEN** a group is collapsed
- **THEN** its members are hidden and the group still shows it has them

### Requirement: A remembered repository can be forgotten

The system SHALL allow a remembered repository, or an imported group, to be
removed from the memory for its host. Removing a group SHALL remove its members
with it. A member of a group SHALL NOT be individually forgettable, and the
attempt SHALL say so rather than doing nothing.

#### Scenario: A remembered repository is removed

- **WHEN** a remembered repository is removed
- **THEN** it is no longer offered for that host, and the repository itself is untouched

#### Scenario: A group is removed

- **WHEN** an imported group is removed
- **THEN** the group and its members stop being offered together

### Requirement: Worktree creation asks which branch to cut from

When any repository is in worktree mode, the system SHALL offer the branches of
the primary repository, refreshed from its remote first so a branch created
elsewhere is offered. The list SHALL lead with the branch a new worktree would
most likely be cut from — the remote's default branch, then the local default —
and the branch names SHALL come from the machine the session will run on.

#### Scenario: Branches are offered

- **WHEN** the base-branch step opens
- **THEN** the repository's branches are listed with the remote and local defaults first

#### Scenario: The refresh fails

- **WHEN** the remote cannot be reached
- **THEN** the branches known locally are still offered

#### Scenario: The repository has no branches

- **WHEN** no branch can be listed
- **THEN** the flow reports it and creates nothing

### Requirement: The session and its branch are named

The system SHALL ask for a session name, and refuse an empty one. When a
worktree is being created it SHALL then ask for the branch name, prefilled with
a branch-safe form of the session name, and refuse an empty one.

#### Scenario: A name is given

- **WHEN** a session name is confirmed
- **THEN** the flow continues and the created session carries that name

#### Scenario: An empty name is given

- **WHEN** the name is confirmed empty
- **THEN** it is refused with a reason and the step stays open

#### Scenario: A worktree branch is proposed

- **WHEN** the branch step opens after a session name was given
- **THEN** it is prefilled with a branch-safe form of that name and remains editable

### Requirement: The agent is chosen last, defaulting to the configured one

The system SHALL offer the agents the launcher itself knows, with the configured
default preselected, as the final step. With one agent or none, the flow SHALL
not ask.

#### Scenario: Several agents are available

- **WHEN** the agent step opens
- **THEN** every registered agent is offered with the configured default preselected

#### Scenario: One agent is available

- **WHEN** only one agent is registered
- **THEN** no question is asked and that agent is used

### Requirement: The flow never blocks the interface

Every part of the flow that touches a disk, a network or a host — listing
directories, checking a typed path, scanning a folder, fetching and listing
branches, and the creation itself — SHALL run off the render path. The flow
SHALL remain interactive throughout, and each wait SHALL be visible where it is
being waited for.

#### Scenario: A host is slow to answer

- **WHEN** a listing, path check or branch listing is in flight against a slow host
- **THEN** the interface keeps redrawing and accepting input, and the wait is shown

#### Scenario: A result arrives after the flow moved on

- **WHEN** a result arrives for a step the user has already left or cancelled
- **THEN** it is discarded and changes nothing

### Requirement: Cancelling leaves nothing behind

The system SHALL allow the flow to be abandoned at any step. Abandoning it
SHALL create no session, leave no placeholder row and make no change that the
user did not explicitly ask for — with the exception of repository memory, which
is written by explicit acts (accepting a typed path, importing a folder,
forgetting one) and survives on purpose.

#### Scenario: The flow is abandoned midway

- **WHEN** the flow is cancelled after choices were made
- **THEN** no session is created and no placeholder remains

#### Scenario: A path was added before cancelling

- **WHEN** a typed path was accepted and the flow is then cancelled
- **THEN** that repository is still remembered for next time

### Requirement: The flow is replaceable

Choosing what to create SHALL be an ordinary plugin over choices the system
exposes, not built-in interface. Everything the bundled flow offers — hosts,
remembered repositories and their groups, directory listings, branch lists,
agents — SHALL be readable by any plugin, and every act it performs SHALL be
expressible as a command.

#### Scenario: The bundled flow is replaced

- **WHEN** a user removes the bundled flow and writes their own
- **THEN** every choice the bundled one offered is available to theirs
- **AND** every change it made is available as a command

#### Scenario: The bundled flow is removed and not replaced

- **WHEN** the bundled flow is removed with nothing in its place
- **THEN** the interface keeps working and its key is left unbound rather than reused
