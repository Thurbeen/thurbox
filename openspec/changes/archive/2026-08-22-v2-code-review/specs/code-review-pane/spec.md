## Purpose

Defines the review surface: how a session's changes are shown, navigated and
exported — and why it is cells rather than a tree.

## ADDED Requirements

### Requirement: A session's changes are readable

The system SHALL expose the diff between a session's worktree and the branch it
was created from, and the diff of its uncommitted changes, as a read.

Computing a diff SHALL NOT block rendering. Until one is ready its absence
SHALL be distinguishable from an empty diff.

#### Scenario: A diff is requested

- **WHEN** a session's changes are read before the diff has been computed
- **THEN** the not-yet-known state is reported, distinct from "no changes"

#### Scenario: A diff becomes available

- **WHEN** the computation finishes
- **THEN** the changes are readable without the interface having stalled

#### Scenario: A session with no changes

- **WHEN** a session's worktree matches its base
- **THEN** an empty diff is reported, distinct from not-yet-known

### Requirement: The changed files are listable

The system SHALL expose, per file in a diff, its path and how many lines were
added and removed.

#### Scenario: Files are listed

- **WHEN** a plugin renders the changed files
- **THEN** each file's path and added/removed counts are available

### Requirement: The review body is a surface

The rendered diff SHALL be expressible as cells rather than as a tree, because
it is positioned by character measurement against a resolved width — side by
side, wrapped, or scrolled horizontally.

#### Scenario: The body is drawn

- **WHEN** a plugin renders the diff body
- **THEN** it supplies cells and the kernel paints them within the resolved rect

#### Scenario: The pane is narrow

- **WHEN** the pane is too narrow for the content
- **THEN** the plugin decides what to show, from the width it was given

### Requirement: The review can be exported to the agent

The system SHALL allow the review — or a selected part of it — to be sent to the
session's agent as text.

#### Scenario: A review is sent

- **WHEN** an export is commanded
- **THEN** the session's agent receives the review as a prompt
