## Purpose

Defines what a plugin may read from the session engine and what it may command
it to do, and the capability model that bounds those powers — so that the whole
user interface can be written in Lua without any plugin gaining filesystem,
process or network access.

## ADDED Requirements

### Requirement: Reads are served from a snapshot and never block

Every read a plugin performs SHALL be served from an in-memory snapshot
maintained by the kernel. A read SHALL return immediately and SHALL NOT wait on
a database query, a subprocess, a network round trip or a remote host.

#### Scenario: A plugin reads while a remote host is unreachable

- **WHEN** a plugin reads the session list while a configured remote host is unresponsive
- **THEN** the read returns immediately from the snapshot

#### Scenario: A plugin reads every frame

- **WHEN** a plugin reads the same data on every render
- **THEN** each read returns immediately and does not degrade the frame rate

### Requirement: Snapshot content is bounded in staleness

The kernel SHALL refresh the snapshot on its own schedule, independent of plugin
invocation. A read MAY return data that is out of date by at most a documented
bound. The system SHALL expose, per snapshot, the point in time it represents,
so a plugin can render staleness rather than misrepresent it as current.

#### Scenario: Underlying state changes

- **WHEN** a session's status changes outside the running application
- **THEN** reads reflect the new status within the documented staleness bound

#### Scenario: A plugin renders freshness

- **WHEN** a plugin reads the snapshot
- **THEN** it can determine the point in time that snapshot represents

### Requirement: Writes are commands that are queued and never block

Every state-changing operation a plugin performs SHALL be expressed as a
command. Issuing a command SHALL return immediately without waiting for the
operation to complete. The system SHALL execute commands off the rendering path.

The effect of a completed command SHALL become visible through a later snapshot.
A plugin SHALL NOT be able to wait for, or synchronously observe, a command's
completion.

#### Scenario: A plugin spawns a session

- **WHEN** a plugin issues a spawn command for a repository requiring a slow clone or a remote connection
- **THEN** the call returns immediately and the interface remains responsive
- **AND** the new session becomes visible in a later snapshot

#### Scenario: A command fails

- **WHEN** a command fails during execution
- **THEN** the failure is reported to the plugin through a subsequent snapshot rather than as an immediate error

### Requirement: In-flight commands are observable

The system SHALL expose commands that have been issued but not yet completed,
including which entity each concerns and what phase it has reached, so that a
plugin can render work in progress rather than an unexplained gap.

#### Scenario: A session is being created

- **WHEN** a spawn command is in flight
- **THEN** a plugin can read that it is in flight and which phase it has reached

### Requirement: Plugins can read the session engine's state

The system SHALL expose to plugins the sessions that exist and their observable
attributes — identity, display name, agent, status, working directory, branch,
backend, parent relationship and manual ordering — together with the repositories
and hosts a session may be created against.

#### Scenario: A plugin renders the session list

- **WHEN** a plugin renders the list of sessions
- **THEN** every attribute needed to reproduce the v1 session list is available from the snapshot

### Requirement: Plugins can command the session lifecycle

The system SHALL allow a plugin to command the full session lifecycle: create,
delete, restore, restart, fork, reorder, select, and send input to a session's
underlying process.

#### Scenario: The session list is a plugin

- **WHEN** the session list is implemented as a plugin
- **THEN** it can perform every session operation v1 offered from its session list, without kernel changes

### Requirement: Capabilities are enforced by absence

A capability a plugin has not been granted SHALL NOT be present in that plugin's
environment. Enforcement SHALL be by absence of the binding rather than by a
runtime check that a present binding refuses.

#### Scenario: An ungranted capability is used

- **WHEN** a plugin references a capability it was not granted
- **THEN** the reference resolves to nothing rather than to a binding that rejects the call

### Requirement: No plugin has filesystem, process or network access

The plugin environment SHALL NOT expose the ability to read or write arbitrary
files, to start or signal processes, or to open network connections. Operations
that inherently require these SHALL be performed by the kernel and exposed as
narrow reads and commands.

#### Scenario: A plugin displays file contents

- **WHEN** a plugin renders the contents of a file
- **THEN** the contents are supplied by the kernel through a read, and the plugin has no general file access

#### Scenario: A plugin opens an external editor

- **WHEN** a plugin opens the user's editor
- **THEN** it issues a command the kernel executes, and the plugin has no general process access

#### Scenario: A plugin attempts direct access

- **WHEN** a plugin attempts to open a file or a socket by any means available in its environment
- **THEN** no such means is present

### Requirement: New capabilities are introduced with their consumer

A capability SHALL NOT be added to the plugin environment before a plugin
requires it. Every granted capability SHALL have at least one consumer at the
time it is introduced.

#### Scenario: A capability is proposed without a consumer

- **WHEN** a capability is proposed and no plugin yet requires it
- **THEN** it is not added until a plugin requiring it exists
