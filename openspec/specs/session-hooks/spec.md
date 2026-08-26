# session-hooks Specification

## Purpose
Lets a user run their own commands at the moments thurbox creates, deletes,
restarts or restores a session — before the operation, where they can refuse
it, and after, where they can react to it — declared as data and fired the same
way whichever interface asked for the operation.
## Requirements
### Requirement: Hooks are declared as data in a config file

The system SHALL read session lifecycle hooks from a `hooks.toml` file beside
the other config files, as a list of entries each naming one **event** and one
**shell command**, with an optional per-entry timeout in seconds. The file
SHALL be seeded with commented-out examples on first run. A missing, empty or
example-only file SHALL mean no hooks, and the system SHALL behave exactly as it
did before hooks existed.

#### Scenario: No hooks file

- **WHEN** no `hooks.toml` exists in the config directory
- **THEN** the system writes one containing only comments
- **AND** every session operation behaves as it did without hooks

#### Scenario: A hook is declared

- **WHEN** `hooks.toml` contains an entry for `session.post_create` with a command
- **THEN** that command runs after every session creation

#### Scenario: The file is malformed

- **WHEN** `hooks.toml` cannot be parsed, or names an event that does not exist
- **THEN** no hooks run
- **AND** a warning naming the file and the problem is reported, the same way a
  malformed `hosts.toml` is
- **AND** `thurbox-cli config validate` reports the file as failing

### Requirement: Eight events cover the four session operations

The system SHALL fire `session.pre_create`, `session.post_create`,
`session.pre_delete`, `session.post_delete`, `session.pre_restart`,
`session.post_restart`, `session.pre_restore` and `session.post_restore`. A
`pre_*` event fires before the operation has any side effect; a `post_*` event
fires after the operation has fully succeeded, and not otherwise.

#### Scenario: A creation that fails part-way

- **WHEN** `session.pre_create` hooks have run and creation then fails (a bad
  branch, an unreachable host)
- **THEN** `session.post_create` does not fire

#### Scenario: A fork is a creation

- **WHEN** a session is forked
- **THEN** the `session.pre_create` and `session.post_create` hooks fire for the
  new session, with the parent's id in the context

#### Scenario: A soft delete and a force delete are both deletes

- **WHEN** a session is soft-deleted from the TUI, or force-deleted from the CLI
- **THEN** the `session.pre_delete` and `session.post_delete` hooks fire, and the
  context says whether it was forced

#### Scenario: An undo is a restore

- **WHEN** a soft-deleted session is brought back, by undo in the TUI or by
  `session restore`
- **THEN** the restore hooks fire

### Requirement: Hooks fire once per operation regardless of the caller

Every way of performing a session operation — the TUI, `thurbox-cli`, an
automation, an extension — SHALL fire the same hooks exactly once for that
operation.

#### Scenario: Creation from three entry points

- **WHEN** a session is created from the TUI's creation flow, from
  `thurbox-cli session create`, and from a `spawn` automation
- **THEN** each creation fires `session.pre_create` and `session.post_create`
  once

### Requirement: Hooks run in declaration order and never on the render path

Hooks for one event SHALL run one at a time in the order they appear in the
file. They SHALL run on the thread performing the operation, which in the TUI
is never the thread that draws the screen.

#### Scenario: Two hooks for one event

- **WHEN** two entries name `session.post_create`
- **THEN** the first has exited before the second starts

#### Scenario: A slow hook in the TUI

- **WHEN** a `session.pre_create` hook takes ten seconds
- **THEN** the interface keeps painting and reports the creation as running its
  hooks, and the creation proceeds when the hook exits

### Requirement: A pre-hook can veto the operation

If any `pre_*` hook exits non-zero, cannot be started, or exceeds its timeout,
the system SHALL abort the operation before it has any side effect — no
worktree, no process, no row changed — and SHALL report the failure with the
hook's command, its exit status (or that it timed out) and the tail of its
stderr. Hooks declared after the failing one for that event SHALL not run.

#### Scenario: A pre-create hook refuses

- **WHEN** a `session.pre_create` hook exits `1` with `refusing: protected
  branch` on stderr
- **THEN** no session is created, no worktree exists, no process was launched
- **AND** the reported error contains `refusing: protected branch`

#### Scenario: A pre-delete hook refuses

- **WHEN** a `session.pre_delete` hook exits non-zero
- **THEN** the session is not deleted and is unchanged in the list
- **AND** the interface reports why

#### Scenario: A pre-hook hangs

- **WHEN** a `session.pre_restart` hook does not exit within its timeout
- **THEN** it is killed, the restart does not happen, and the report says the
  hook timed out

### Requirement: A post-hook cannot fail the operation

A `post_*` hook's exit status SHALL never change the outcome of the operation.
Every `post_*` hook for the event SHALL run even if an earlier one failed. A
failure SHALL be logged and SHALL be carried in the operation's report where
the caller exposes one.

#### Scenario: A post-create hook fails

- **WHEN** a `session.post_create` hook exits `2`
- **THEN** the session exists and is running
- **AND** the CLI's JSON for the create names the failed hook
- **AND** the failure is in the log

#### Scenario: A post-hook hangs

- **WHEN** a `session.post_delete` hook exceeds its timeout
- **THEN** it is killed, the delete stands, and the timeout is reported as a
  hook failure

### Requirement: A hook receives the session's facts as environment and as JSON

Each hook SHALL receive `THURBOX_HOOK_EVENT` (the event name) and, as far as
they are known at that point, `THURBOX_SESSION` (the session id),
`THURBOX_SESSION_NAME`, `THURBOX_AGENT`, `THURBOX_REPO` (the primary
repository path), `THURBOX_CWD` (the directory the agent runs in),
`THURBOX_BRANCH`, `THURBOX_BASE_BRANCH`, `THURBOX_HOST` (the remote host name,
unset for a local session), `THURBOX_PARENT_SESSION` and `THURBOX_TASK`. The
same facts, plus the list of worktrees and additional directories, SHALL be
written to the hook's stdin as one JSON object, after which stdin is closed. A
hook SHALL inherit the same config- and data-directory overrides the agent
inside a session receives, so a `thurbox-cli` it runs addresses the database of
the thurbox that fired it.

#### Scenario: A post-create hook reads its environment

- **WHEN** `session.post_create` fires for a worktree session on branch `feat/x`
- **THEN** `THURBOX_BRANCH` is `feat/x`, `THURBOX_CWD` is the worktree path,
  `THURBOX_REPO` is the repository it was made from, and `THURBOX_SESSION` is
  the id `thurbox-cli session get` accepts

#### Scenario: A pre-create hook sees what will be created

- **WHEN** `session.pre_create` fires
- **THEN** `THURBOX_SESSION_NAME`, `THURBOX_AGENT`, `THURBOX_REPO` and the
  requested branch are set
- **AND** `THURBOX_SESSION` is the id the session will have if creation succeeds

#### Scenario: A hook calls thurbox-cli

- **WHEN** a hook fired by a dev build runs `thurbox-cli session list`
- **THEN** it lists that dev build's sessions, not the release build's

#### Scenario: The JSON payload

- **WHEN** a hook reads its stdin
- **THEN** it gets one JSON object whose `event` matches `THURBOX_HOOK_EVENT` and
  whose `worktrees` lists each worktree's repository, path and branch

### Requirement: A hook runs in the session's repository with no terminal

A hook SHALL run through the platform shell (`sh -c`, or `cmd /C` on Windows)
with its working directory set to the session's primary repository when that is
a directory on the local machine, and otherwise to the working directory of the
thurbox process. It SHALL not inherit thurbox's stdin, stdout or stderr: its
output is captured and tail-truncated for reporting, and it can neither draw on
nor read from the terminal the TUI owns.

#### Scenario: A hook for a local worktree session

- **WHEN** `session.post_delete` fires for a local session whose worktree was
  removed
- **THEN** the hook's working directory is the repository the worktree was made
  from, which still exists

#### Scenario: A hook for a remote session

- **WHEN** any hook fires for a session on an SSH or WSL host
- **THEN** the hook runs on the local machine, `THURBOX_HOST` names the host,
  and `THURBOX_CWD` is the path on that host

#### Scenario: A chatty hook

- **WHEN** a hook prints a megabyte and exits non-zero
- **THEN** the TUI's screen is undisturbed and the report carries only the tail
  of the output

### Requirement: The config surface knows the file

`thurbox-cli config validate` SHALL strict-parse `hooks.toml` alongside the
other config files, and `thurbox-cli config show` SHALL print its path and the
hooks in force.

#### Scenario: Validate on an unknown field

- **WHEN** an entry in `hooks.toml` has a misspelt field
- **THEN** `config validate` exits non-zero and names `hooks.toml`

#### Scenario: Show the hooks in force

- **WHEN** `config show` runs with two hooks declared
- **THEN** its output lists both, each with its event and command
