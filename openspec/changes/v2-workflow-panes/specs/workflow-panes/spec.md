## Purpose

Defines the task and automation surfaces: what they show, how they are edited,
and how triggering one reaches an agent — so that work thurbox already schedules
headlessly is visible and actionable from the interface.

## ADDED Requirements

### Requirement: Tasks are readable and editable

The system SHALL expose every task with its title, description, status and
origin. A plugin SHALL be able to create a task, change its title or
description, cycle its status, and delete it.

#### Scenario: The task list is rendered

- **WHEN** a plugin renders the tasks
- **THEN** each task's title, status and origin are available

#### Scenario: A task's status is changed

- **WHEN** a status change is commanded
- **THEN** the new status appears in a later snapshot

#### Scenario: A task from an external tracker

- **WHEN** a task originates from a tracker rather than locally
- **THEN** its origin is distinguishable from a local one

### Requirement: A task can be handed to an agent

The system SHALL allow a task to be acted on by an agent, either by sending it
to a running session or by creating one for it. The agent SHALL receive the
task's full context — its identifier, title and description — not only its
title.

Acting on a task SHALL advance it out of the not-started state.

#### Scenario: A task is sent to a running session

- **WHEN** a task is dispatched to an existing session
- **THEN** that session's agent receives the task's identifier, title and description
- **AND** the task is no longer in the not-started state

#### Scenario: A task is given a new session

- **WHEN** a task is dispatched to a session that does not exist yet
- **THEN** a session is created and the task's context reaches it once it is ready

### Requirement: Automations are readable and controllable

The system SHALL expose every automation with its name, schedule, action,
enabled state and last outcome. A plugin SHALL be able to enable or disable one,
run it immediately, and delete it.

#### Scenario: The automations are rendered

- **WHEN** a plugin renders the automations
- **THEN** each one's name, schedule, enabled state and last outcome are available

#### Scenario: An automation is run on demand

- **WHEN** an immediate run is commanded
- **THEN** the run happens off the render path and its outcome appears in the history

#### Scenario: An automation is disabled

- **WHEN** an automation is disabled
- **THEN** it stops firing on its schedule and the change persists

### Requirement: An automation's run history is readable

The system SHALL expose recent runs of an automation — when each ran, whether it
succeeded, and what it reported.

#### Scenario: A run is inspected

- **WHEN** a plugin renders an automation's history
- **THEN** each run's time, outcome and detail are available

### Requirement: These panes are plugins

The task and automation surfaces SHALL be plugins over reads and commands, with
no kernel knowledge of either concept beyond carrying their data.

#### Scenario: A pane is replaced

- **WHEN** a user replaces the bundled task pane
- **THEN** every read and command the bundled one used is available to theirs
