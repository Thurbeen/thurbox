---
name: task-status
description: Inspect one Task's state and worker evidence or coordinate directly with an existing owned Task session. Load for completion checks, steering, unknown state or recovery investigation; use status-digest for routine status.
license: MIT
---

# Report current Task state

Use the exact Mission Control configuration for every operation, explicitly
with `fleet --config PATH` or inherited `FLEET_CONFIG`. Routine status is the
status-digest board; open one Task only when a line needs a closer look:

```text
fleet status task NAME
fleet get task NAME
```

Summarize what you read in a line or two; never paste the resource.

Read phase, conditions, observedGeneration, brief receipt, session reference and
result together. A phase alone is not sufficient evidence. Compare observations
with the current spec generation, and the result's generation with the delivered
brief; stale observations stay stale.

Mission Control verifies the evidence relevant to the promised result. Do not redo
the worker's substantive assignment. Delegate an independent review or
diagnostic when verification itself requires substantive work. Routine Task and
change-request status reporting may use the minimal live checks in this skill.

| Observation | Report or action |
| --- | --- |
| Pending with dispatch false | Work has not been authorized to start. |
| Spawning or Spawned | Startup is in progress; inspect conditions and receipts. |
| Briefed | Brief delivery is recorded; completion is not yet recorded. |
| Stalled or NeedsAttention | Explain the recorded condition. Check the runtime before proposing recovery. |
| Unknown or disconnected runtime | State is unknown. Do not create a replacement blindly. |
| Concluded with a result | A worker recorded a claim. Check the result and artifact evidence. |
| Releasing | Deletion is waiting for session cleanup; do not bypass the finalizer. |

Inspect CLI errors and exit codes; never turn an unreadable store into an empty
queue. Never poll or wait in the foreground, and do not compete with the
engine's manager: changes reach you as Fleet messages in your inbox.

## Execution, inbox and declared changes

`status.execution` holds the frozen harness, host, model and source resources;
resolved values stay in private launch snapshots, and current TaskConfig or
EnvProvider specs may differ from it.

Read `fleet send --help` and `fleet declare --help`. `fleet send NAME "MESSAGE"`
queues a message; success means queued, not delivered. Inspect `status.inbox`,
`status.deliveries` and delivery conditions for receipts or failure. The controller
delivers oldest first to ready sessions, including retained Concluded sessions.
A missing concluded worker raises WorkerGone; it is never respawned for delivery.
Do not replace the inbox through apply or edit status directly.

## Direct coordination with an existing session

Mission Control may send an existing owned Task session a clarification,
in-brief steering, readiness or status request, scoped handoff, or timely update.
This is orchestration, including during a working turn when the runtime has a
verified native noninterrupting route. It is not permission to perform the
substantive assignment, impersonate the worker, edit its workspace or result,
interrupt it for convenience, or bypass Task ownership. Fleet owns initial brief
delivery: never resend the brief, forge a receipt, edit status, or spawn a duplicate.

Use this guarded workflow:

1. Read current help for `fleet get`, `fleet status` and `fleet send`. Resolve
   the Task with `fleet --config /path/to/fleet.toml get task TASK` and
   `fleet --config /path/to/fleet.toml status task TASK --json`. Take the UUID,
   runtime host and logical key only from `status.session`; never type a guessed
   name or select whichever session looks idle.
2. Read the same config's `runtime.kind` and `runtime.cli`. Use that configured
   executable and its current help; do not assume it is named `thurbox-cli`.
   FakeRuntime does not model terminal interaction, and a runtime or remote host
   without documented shared-session access is unsupported: use the durable
   Fleet inbox or report the direct path blocked instead of inventing commands.
3. For Thurbox, read `session get`, `session capture`, `session send`, `session
   key` and any event/wait help that the installed version actually provides.
   Verify the UUID's live host, expected agent/foreground process, workspace and
   key ownership. Fleet's logical session key and Thurbox's physical key can use
   different configured prefixes; validate the documented `runtime.keyPrefix`
   mapping and Task uid rather than requiring the two strings to be identical.
4. Immediately before sending, inspect a fresh current pane and cursor with
   `session capture --lines 0 --json`, plus live `session get` when needed.
   Unknown or wrong identity, stale scrollback, an unexpected foreground shell,
   an unreachable or unshared host, a permission/credential/trust dialog, or
   occupied input with no safe native route means blocked or queued. Never paste
   coordinator text into a dialog or over user typing.
5. Pass the prompt as one literal argument, preserving quotes and newlines. On a
   compatible Thurbox release the generic shape is:

   ```text
   <RUNTIME_CLI> session get UUID --json
   <RUNTIME_CLI> session capture UUID --lines 0 --json
   <RUNTIME_CLI> session send UUID TEXT --json --strict
   ```

   Do not default to `--force`, clear the composer, press Escape or Ctrl-C, or
   kill/interrupt a turn merely to deliver coordination. A key press is narrower.
   Capture again after `--no-enter`; verify the exact coordinator prompt remains
   in the same composer, identity and foreground are unchanged, and no dialog or
   shell replaced it. Only then may `session key UUID enter --json` submit that
   known text. If any check is uncertain, do not press a key. It must not approve
   a trust or permission prompt. Do not wait for the worker's answer: end your
   turn, and its declarations and result reach your inbox through Fleet.

Read the runtime JSON and process exit code together:

| Evidence | Meaning |
| --- | --- |
| `sent: true` | Text was typed through the runtime transport; it is not proof the worker accepted it. |
| `submitted: true` | The runtime observed the line leave the terminal composer. `false` is not terminal submission; `null` is unknown. |
| `delivered_via` | Names the actual route. A native agent inbox may safely steer a working turn; `mailbox` is only queued. |
| Exit code 0 | The CLI met its command contract. It does not prove acceptance, activity or completed work. |
| Fresh output or a working-edge event | May corroborate acceptance/activity. It never proves completion. |

A strict failure or wait timeout can still follow a partial or successful side
effect. Unknown submission is not permission to send the same prompt again with
`fleet send`. Reconcile the returned JSON, a fresh capture/event and Task state,
report uncertainty, then decide whether a retry is safe. Choose one route for one
prompt: do not direct-send and durable-send the same instruction as a fallback.

Keep `fleet send TASK TEXT` as the durable Task-inbox path when queued delivery is
desired or direct interaction is unsafe; supply the exact config as in
`fleet --config /path/to/fleet.toml send TASK TEXT`. It waits for a ready session
through the controller and can produce a Fleet delivery receipt; CLI success
initially means queued. A direct runtime send creates no Fleet inbox entry or
Fleet delivery receipt. If accountability is needed, record it in Fleet on your
own Task, without sensitive prompt text:
`fleet declare vega --json '{"kind":"Note","text":"steered NAME: outcome"}'`.
Never mutate Task status or keep the record in agent memory.

Retained Concluded sessions may receive follow-up coordination, but their immutable
result is not replaced. A new substantive request requires a new Task.

`fleet declare NAME --change-request URL` ensures a ChangeRequest mirror and
appends a worker declaration. Inspect `status.declared`, `status.forge` and
`fleet get changerequest MIRROR` for observations, gaps and conditions. The
operator must configure the URL's forge host and authenticate its CLI first.
An arbitrary fact can be recorded with
`fleet declare NAME --json '{"kind":"Note","text":"Verified locally"}'`;
it concludes nothing. Neither declaration is a final result.

Forge feedback is relayed into the inbox. Ready transitions depend on configured
policy, worker completion and idle state; Fleet never merges a change. Missing
observations and stale heads are not proof that checks or review passed.
ChangeRequest mirrors outlive Tasks; deleting a Task does not delete its changes
or mirrors. Do not declare or conclude another worker's work on its behalf.

## Worker completion

Read `fleet result --help` and the Task's brief receipt. A worker records its own
answer once, with the generation in `status.brief.generation`, even if a later
dispatch or forge edit advanced `metadata.generation`:

```text
fleet result NAME --generation 1 --outcome shipped --summary "Verified result"
```

Replace the example generation and summary with actual evidence. Outcomes are
`shipped`, `stuck`, `failed` or `not-applicable`. Supply declared structured outputs
and authorized artifact URLs when required. Do not report another worker's result
for it. Fleet stores the claim; it is not proof that a PR, CI or deployment succeeded.
Forge observations provide separate evidence; inspect them when configured.
For a user-authorized pull or merge request deliverable, verify a real declared
URL and the requested base. Verify everything required by the user or repository
for that delivery stage, including checks, CI and review. A local commit, an
undeclared change or blocked required gates do not satisfy that result. A
draft-only deliverable
does not require later ready/merge gates unless requested. Do not infer publication
authorization for Tasks whose requested result did not include it.
After reconciliation, Concluded keeps its session until deletion. Delete only
when the user authorizes cleanup and the conversation no longer needs it.
Deleting a Task deletes its session, and the runtime then cleans the workspace
as configured: by default Thurbox removes a `repositories` Task's worktrees and
branch, unpushed commits included. Confirm the work is pushed or no longer
needed first. A `dir` Task's directory stays.
