## Purpose

Defines the behaviours around a live terminal that make it usable rather than
merely visible: copying what is on screen, opening a link, being told when an
agent needs you, and a shell beside the agent.

## ADDED Requirements

### Requirement: Text on screen can be copied

The system SHALL allow the visible contents of a session's terminal, or a
selected part of it, to be placed on the clipboard.

Where the clipboard cannot be reached directly — a remote host, a bare tty — the
system SHALL fall back to the terminal's own copy mechanism rather than failing
silently.

#### Scenario: A copy is requested

- **WHEN** a copy is commanded for a session
- **THEN** the visible terminal contents are placed on the clipboard

#### Scenario: No native clipboard is available

- **WHEN** the clipboard cannot be reached directly
- **THEN** the fallback mechanism is used and the outcome is reported

### Requirement: Links in terminal output can be opened

The system SHALL detect links in a session's visible output and allow one to be
opened. Where no browser can be reached, the link SHALL be copied instead, and
the choice reported.

#### Scenario: Output contains a link

- **WHEN** a session's visible output contains a URL
- **THEN** the links present are readable, each with where it appears

#### Scenario: A link is opened with no browser available

- **WHEN** opening is commanded on a host with no browser
- **THEN** the link is copied instead and that is reported

### Requirement: Notifications fire when an agent needs attention

The system SHALL raise a desktop notification when a session becomes blocked,
subject to the user's notification settings, and SHALL not raise one for the
session currently in view when suppression is configured.

#### Scenario: A session becomes blocked

- **WHEN** a session transitions to blocked and is not the one in view
- **THEN** a notification is raised

#### Scenario: Notifications are disabled

- **WHEN** notifications are turned off
- **THEN** none is raised and no delivery is attempted

### Requirement: A shell can be opened beside the agent

The system SHALL allow a shell to be run in a session's working directory and
shown as a surface, distinct from the agent's own terminal.

#### Scenario: A shell is opened

- **WHEN** a shell is requested for a session
- **THEN** it runs in that session's working directory and its output is shown

### Requirement: These are plugin-facing, not kernel UI

Copying, link opening and the shell SHALL be reads and commands, with the
surfaces that present them being plugins.

#### Scenario: A pane is replaced

- **WHEN** a user replaces the pane offering these
- **THEN** the same reads and commands are available to theirs
