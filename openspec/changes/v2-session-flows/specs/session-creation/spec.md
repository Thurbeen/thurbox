## Purpose

Defines how a session is brought into existence, forked, synced with its base
branch and restored after deletion — and what a plugin can render while any of
that is happening, since none of it is fast.

## ADDED Requirements

### Requirement: Creating a session is a command like any other

The system SHALL accept a create command naming a repository, and optionally a
branch to create a worktree on, an agent, and a host. The call SHALL return
immediately, before any repository, worktree or process work begins.

#### Scenario: Creation is requested

- **WHEN** a plugin issues a create command
- **THEN** the call returns at once and the work proceeds off the render path

#### Scenario: The repository does not exist

- **WHEN** the named repository cannot be resolved
- **THEN** the failure is reported through a later snapshot, not as an immediate error

### Requirement: Creation progress is renderable throughout

Creation involves work that takes tens of seconds on a large repository — a
fetch, a worktree checkout, a remote connection, a process launch. The system
SHALL expose which phase the work has reached, and which repository and branch
it concerns, from the moment the command is accepted until the session appears.

A plugin SHALL be able to render a placeholder for a session that does not exist
yet, positioned where the real one will appear.

#### Scenario: A long creation is in flight

- **WHEN** creation has been accepted but the session does not yet exist
- **THEN** the phase and the repository it concerns are readable
- **AND** they remain readable across every phase until the session appears

#### Scenario: Creation fails midway

- **WHEN** creation fails after some work has been done
- **THEN** the failure and the phase it failed in are readable
- **AND** no half-created session appears in the session list

### Requirement: A session can be forked

The system SHALL accept a fork command naming an existing session, producing a
new session that records the original as its parent and starts from the same
working state.

#### Scenario: A running session is forked

- **WHEN** a fork is requested for an existing session
- **THEN** a new session appears whose parent is the original

#### Scenario: Forking something that is gone

- **WHEN** a fork names a session that no longer exists
- **THEN** the failure is reported and nothing is created

### Requirement: A session's worktree can be synced with its base

The system SHALL accept a sync command that brings a session's worktree up to
date with the branch it was created from, reporting what happened — including
when the working tree has changes that prevent it.

#### Scenario: A clean worktree is synced

- **WHEN** sync is requested for a session with no local modifications
- **THEN** the worktree is updated and the outcome is reported

#### Scenario: Sync cannot proceed

- **WHEN** the worktree has changes that would be lost
- **THEN** the sync does not proceed and the reason is reported

### Requirement: A deleted session can be restored

The system SHALL expose sessions that have been deleted but not purged, and
accept a command to restore one. A session deleted with its worktree removed
SHALL be restorable only on an explicit best-effort basis, and the system SHALL
make that distinction visible before it is chosen.

#### Scenario: A soft-deleted session is restored

- **WHEN** restore is requested for a soft-deleted session
- **THEN** it returns to the session list

#### Scenario: A force-deleted session is offered

- **WHEN** the deleted sessions are listed
- **THEN** those whose worktree was removed are marked as recoverable only in part

### Requirement: The creation flow is a plugin

Choosing what to create — the repository, the branch, the agent, the host — SHALL
be a plugin rendering choices the kernel exposes, not kernel UI. The kernel SHALL
expose the repositories, agents and hosts available to create against.

#### Scenario: The flow is replaced

- **WHEN** a user replaces the bundled creation flow with their own
- **THEN** every choice the bundled one offered is available to theirs

#### Scenario: No hosts are configured

- **WHEN** no remote hosts are configured
- **THEN** the flow offers the local host without requiring a choice
