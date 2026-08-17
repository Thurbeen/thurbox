## Purpose

Defines the surfaces for finding your way around: what a session's details show,
how files are browsed without giving plugins filesystem access, and how a search
spans everything at once.

## ADDED Requirements

### Requirement: A session's details are readable

The system SHALL expose, for any session, the information needed to describe it:
its agent, working directory, branch, backend, parent, worktrees, and the state
of its git working tree when that is known.

#### Scenario: A session is inspected

- **WHEN** a plugin renders a session's details
- **THEN** its agent, directory, branch and backend are available

#### Scenario: Git state is not yet known

- **WHEN** a session's working-tree state has not been computed
- **THEN** its absence is distinguishable from a clean tree

### Requirement: Files are browsed without filesystem access

The system SHALL expose the contents of a session's working directory — the
entries at a path, and the text of a file — as reads. A plugin SHALL NOT gain
general filesystem access in order to browse.

A read outside the session's working directory SHALL be refused.

#### Scenario: A directory is listed

- **WHEN** a plugin asks for the entries at a path inside a session's directory
- **THEN** the entries are returned, each marked as a file or a directory

#### Scenario: A file is opened

- **WHEN** a plugin asks for a file's text
- **THEN** the text is returned, bounded in size

#### Scenario: An attempt to escape the directory

- **WHEN** a plugin asks for a path outside the session's working directory
- **THEN** the read is refused and nothing is returned

### Requirement: Search spans every kind of thing at once

The system SHALL support searching sessions, tasks and automations together,
matching on the text a user would recognise — a session's name, branch and
agent; a task's title and description; an automation's name.

Activating a result SHALL move the interface to the thing found.

#### Scenario: A query matches several kinds

- **WHEN** a query matches a session and a task
- **THEN** both are offered, each identifiable by kind

#### Scenario: A result is activated

- **WHEN** a result is activated
- **THEN** the pane owning it is focused and the thing is selected

#### Scenario: The query matches nothing

- **WHEN** no result matches
- **THEN** that is reported rather than shown as an empty list of unclear meaning
