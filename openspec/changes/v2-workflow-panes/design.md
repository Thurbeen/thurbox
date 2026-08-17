## Context

See `proposal.md`. Tasks and automations are fully functional headlessly — the
storage, the scheduler and `thurbox-cli` all work — so this change adds reads,
commands and panes, and touches none of that machinery.

## Goals / Non-Goals

**Goals:**

- Tasks and automations readable from the snapshot and mutable by command.
- Both panes as bundled plugins, declaring their keys through the registry.
- A second real occupant of the centre slot, which is what actually exercises
  focus-claim.

**Non-Goals:**

- The automation *editor*. Authoring a cron expression and an action is a form,
  and forms need an input primitive with real editing behaviour; `thurbox-cli
  automation create` remains the way to author one until that exists.
- Markdown rendering of task descriptions. v1 rendered them with a Rust
  markdown pass; here that is either a userland widget or a surface, and neither
  is worth deciding before the pane exists.

## Decisions

### D1 — Task dispatch reuses the prompt the CLI already builds

`Task::agent_prompt()` composes the id, title, description and the self-service
hints that let an agent close the task out. Both the send path and the
create-a-session path use it unchanged, so an agent gets identical context
however it was handed the work — and there is one place to change what an agent
is told.

### D2 — Automations are read-only to edit, controllable to run

Enable, disable, run-now and delete are commands. Editing schedule and action is
not, for now: it needs a form, and a half-built form that can write a broken
cron expression is worse than sending someone to the CLI.

*Alternative considered.* A minimal editor with free-text fields. Rejected:
validation lives in the scheduler, and a pane that can persist something the
scheduler will reject is a trap.

## Risks / Trade-offs

**Two more centre-slot occupants make the switch ring long.** → They are
ordinary occupants; if cycling becomes tedious that is an argument for a
selector, which the published occupant list already supports.

**Running an automation on demand can be slow.** → It is a command like any
other, so it runs off the render path and its outcome arrives in the history.
