## Purpose

The host's database owns the sessions that run on that host, whoever created
them; a thurbox reaching the host from elsewhere mirrors that database and asks
the host's own CLI to create, delete, restart or restore — so a session is kept
and handled the same way from the host and from afar.

## ADDED Requirements

### Requirement: A session's record lives on its host

For a shareable host, the system SHALL treat the host's own thurbox database as
the authoritative record of every session running on that host — its id,
name, agent, agent session id, working directory, additional directories,
worktrees, parent, base branch, hook status and deletion state. A remote
thurbox SHALL hold, for each such session, a local row with the **same id** on
its own name for that host's backend (`ssh:<name>` / `wsl:<name>`), and SHALL
treat that row as a mirror: facts and deletion state come from the host, while
display order and the companion shell remain the observer's own.

#### Scenario: A host-native session appears remotely

- **WHEN** a thurbox running on host H creates session `foo`, and a thurbox
  reaching H as `ssh:H` next mirrors H
- **THEN** the remote thurbox holds `foo` with the same session id, backend
  `ssh:H`, and H's paths as its cwd and worktrees, and it attaches to `foo`'s
  window

#### Scenario: A remotely-created session appears on the host

- **WHEN** a thurbox reaching H as `ssh:H` creates session `bar` there
- **THEN** H's database holds `bar`, and a thurbox running on H lists it as a
  local session without any action of its own

#### Scenario: A `thurbox-cli` inside the session finds its own row

- **WHEN** a session created from afar on H runs `thurbox-cli session signal`
  or `message send` on H
- **THEN** the command finds the session's row in H's database by the injected
  `THURBOX_SESSION`

### Requirement: Remote operations are performed by the host's CLI

For a shareable host with a usable CLI, creating, deleting (soft or forced),
restarting and restoring a session on that host SHALL be performed by running
`thurbox-cli` on the host, with the host's own agent definitions and
configuration, and the result SHALL be reflected in the caller's own rows from
the host's answer. Every caller of those operations SHALL delegate the same
way, whether the request came from the interface, the CLI, an automation or an
extension. A failure on the host SHALL be reported to the caller with the
host's own message.

#### Scenario: Create from the interface

- **WHEN** the creation flow picks host H and a repository on it
- **THEN** the session is created by `thurbox-cli` on H — worktree, hooks and
  launch included — and appears in the caller's list with the id H assigned

#### Scenario: Delete from afar

- **WHEN** a remote observer force-deletes a session on H
- **THEN** H's CLI tears down the window and worktrees and marks the row, and
  the observer's row is marked from H's answer

#### Scenario: The host refuses

- **WHEN** H's CLI rejects the operation (an unknown agent on H, a parent not
  on H, a missing repository)
- **THEN** the caller sees H's message and nothing is changed on either side

#### Scenario: The agent is the host's

- **WHEN** a session is created on H with agent `claude`
- **THEN** the definition used is the one in H's `agents.toml`, and a name H
  does not define is refused by H

### Requirement: The host's CLI is provisioned when absent

On first use of a shareable host, the system SHALL probe for a `thurbox-cli`
of the same major version — the one a thurbox running on the host advertises
in thurbox's own directory there, then the host's PATH. Every running thurbox
SHALL advertise its own CLI in that directory, so a host that runs thurbox at
all is shareable without provisioning. When none is found, the system SHALL
obtain the release artifact
matching the host's platform, verify it against the release checksums, place
`thurbox-cli` in thurbox's own directory on the host and use it from there.
When no artifact can be obtained (a development build with no release, a
platform no artifact is shipped for, no network), the host SHALL be used
exactly as it is today — remote worktree creation, the hooks rewrite, the pane
option status channel — and the session SHALL carry a visible note saying
sharing is off for that host and why. Provisioning SHALL never run on the
render path and SHALL never block the interface's start.

#### Scenario: The host runs thurbox itself

- **WHEN** a thurbox — a release install or a development checkout — has run
  on H at least once
- **THEN** a peer's probe finds H's own CLI before anything on PATH, and
  nothing is provisioned as long as it is compatible

#### Scenario: A Linux host with nothing installed

- **WHEN** H has tmux and git but no thurbox, and a session is first created
  on H
- **THEN** `thurbox-cli` is provisioned under thurbox's directory on H before
  the session is created, and H's database is created by that first creation

#### Scenario: A later full install finds the sessions

- **WHEN** the user later installs thurbox on H and starts it
- **THEN** every session created through the provisioned CLI is in its list

#### Scenario: An installed CLI is preferred

- **WHEN** H already has `thurbox-cli` of the same major on its PATH
- **THEN** it is used and nothing is downloaded

#### Scenario: No artifact for the host

- **WHEN** the running build has no release artifact for H's platform
- **THEN** the session is created the way it is today and its info shows
  sharing is off for H with the reason

#### Scenario: A major mismatch

- **WHEN** H's `thurbox-cli` is a different major version than the caller
- **THEN** a matching one is provisioned under thurbox's directory and used
  in preference; H's own install is not touched

### Requirement: A host that cannot be made usable is asked less and less often

A host that answers "no usable CLI" SHALL be left alone for a growing interval
before it is probed again — the base interval on the first failure, doubling
with each consecutive one up to a ceiling — and the count SHALL reset the
moment the host answers usably. Obtaining and shipping the release artifact
SHALL never leave a transport process behind: whatever the outcome of the
transfer, the process fronting it SHALL be reaped before the attempt returns,
and the reported reason SHALL be what the host said rather than the local end
noticing the connection close.

#### Scenario: A host that can never be provisioned

- **WHEN** H fails provisioning for a reason that will not change on its own —
  its shell will not accept a payload that size, no artifact exists for its
  platform
- **THEN** the retry interval grows to the ceiling instead of re-downloading
  the release archive and opening a connection on the base interval for as
  long as thurbox runs

#### Scenario: A host that was only briefly away

- **WHEN** H is unreachable for one pass and answers on the next
- **THEN** it is asked again at the base interval, and its verdict is cached
  as usable with no penalty carried forward

#### Scenario: The transfer dies mid-payload

- **WHEN** the host's end exits while the artifact is being streamed to it
- **THEN** the attempt reports the host's own stderr, and no transport process
  survives the attempt

### Requirement: Hosts are mirrored on a cadence and after every operation

The system SHALL mirror every shareable host on a slow cadence from a worker,
whether or not the observer already holds a session there, and immediately
after each operation it delegated to that host. A mirror pass SHALL adopt rows
new on the host, update facts and hook status of rows it holds, soft-delete
rows the host reports deleted (marked recoverable only in part when the host
says so), and restore rows the host reports active again. An unreachable host
SHALL be skipped for that pass and tried again on the next; the first pass
SHALL NOT delay the interface's start or any frame. The headless heartbeat
tick SHALL run the same pass. A host with `share_sessions = false` in
`hosts.toml` SHALL be neither mirrored nor delegated to.

#### Scenario: A host the observer has never used

- **WHEN** `hosts.toml` names H, the observer holds no session on H, and H's
  database has sessions
- **THEN** within one mirror interval they appear in the observer's list

#### Scenario: The host is down at startup

- **WHEN** H is unreachable when the interface starts
- **THEN** the interface starts and paints without waiting on H, and H is
  mirrored once it answers

#### Scenario: Sharing switched off for a host

- **WHEN** H's entry sets `share_sessions = false`
- **THEN** H is used exactly as today and no row is mirrored from it

#### Scenario: Headless mirror

- **WHEN** the heartbeat tick runs with the interface closed
- **THEN** sessions on the mirrored hosts appear in `session list` afterwards

### Requirement: Mirroring is available as a CLI command

The system SHALL provide `thurbox-cli session sync [--host <name>]`, which
runs one mirror pass for one host or every shareable host and reports which
rows were adopted, updated, deleted and restored, as JSON when piped. The
system SHALL provide `thurbox-cli session list --deleted`, listing the deleted
sessions with their recoverable-in-part mark, which is what a mirroring peer
reads.

#### Scenario: Explicit sync

- **WHEN** `session sync --host H` runs after a peer created a session on H
- **THEN** the output lists that session as adopted and `session list` shows
  it

### Requirement: Status reaches every observer

A session's hook-reported status SHALL reach every thurbox holding that
session. On a shareable host the agent's hooks write the host's database, from
which the mirror carries the status; on a tmux host a status reported through
the CLI from inside a pane SHALL also be recorded on that pane, so a remote
observer's live subscription still shows it within a second. On a Windows
(psmux) host status SHALL arrive through the mirror, which replaces the
`Hooks: degraded` state such hosts show today.

#### Scenario: A host-native session's status seen remotely

- **WHEN** a session on H reports `blocked` through the CLI on H
- **THEN** a thurbox reaching H as `ssh:H` shows it blocked — within a second
  on a tmux host, within a mirror interval on a psmux host

#### Scenario: A report does not echo

- **WHEN** one report reaches an observer through both the mirror and the pane
- **THEN** the session's state is written once and an acknowledged `done` is
  not resurrected

### Requirement: The host relaunches its own sessions

After a host restart, or whenever a session's window is gone, the host's own
thurbox (its interface or its heartbeat tick) SHALL relaunch the session as it
does for local sessions today. A remote observer SHALL NOT launch an agent for
a mirrored row itself; when a completed survey shows a mirrored row with no
window, it SHALL ask the host to relaunch it, and the host SHALL do so only if
the session still has no window, so two observers asking produce one launch.
A window killed outside thurbox is indistinguishable from a crash and is
relaunched by the same rule.

#### Scenario: Reboot with a thurbox on the host

- **WHEN** host H restarts and a thurbox on H and one reaching H as `ssh:H`
  both hold `foo`
- **THEN** `foo` is relaunched once, by H, and both attach to it

#### Scenario: Reboot with no thurbox running on the host

- **WHEN** host H restarts and only a remote observer holds `foo`
- **THEN** the observer asks H's CLI to relaunch `foo`, H does, and a second
  observer asking finds it already running and launches nothing

#### Scenario: The worktree is gone

- **WHEN** a relaunch is asked for a session whose checkout no longer exists
  on H
- **THEN** H refuses naming the directory and the observer shows the row
  failed rather than launching

### Requirement: A deletion can be undone from either side

Undoing a deletion within the undo window on the thurbox that deleted SHALL
leave no trace anywhere. Once the host records a session deleted, every
observer's mirror SHALL show it deleted; a restore performed on any side SHALL
run on the host — recreating the checkout when it can, relaunching when it is
still there — and every observer's mirror SHALL show it active again. A
restore of a session whose checkout the host removed SHALL be offered as
recoverable only in part, and confirmed, exactly as a local force-deleted one
is.

#### Scenario: Undo within the window

- **WHEN** a session on H is deleted from afar and undone before its agent is
  reaped
- **THEN** H's record is unchanged throughout and no other observer notices

#### Scenario: A peer restores what the other side deleted

- **WHEN** a thurbox on H soft-deletes `foo` and a thurbox reaching H restores
  it
- **THEN** H's CLI restores `foo` — relaunching it in its surviving checkout —
  and H's own list shows it active again

#### Scenario: A force-deleted session is restored

- **WHEN** a session H force-deleted is restored from afar with best effort
- **THEN** H recreates the checkout from the surviving branch and relaunches,
  as a local best-effort restore does

### Requirement: Windows hosts are shared through the same path

A Windows SSH host (`multiplexer = "psmux"`) SHALL be shareable: the CLI probe,
provisioning (the Windows release archive) and every delegated command SHALL
go through the PowerShell command path the system already uses for such hosts,
and the host CLI SHALL launch the agent with its ordinary local status hooks.
The remote hooks rewrite SHALL NOT be used on a shared Windows host.

#### Scenario: Create on a Windows host

- **WHEN** a session is created on a psmux host that has, or is provisioned
  with, `thurbox-cli`
- **THEN** it is created by that CLI, its hooks call the host's own
  `thurbox-cli session signal`, and its status reaches the observer through
  the mirror instead of showing `Hooks: degraded`
