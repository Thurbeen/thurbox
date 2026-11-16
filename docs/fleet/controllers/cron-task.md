# CronTask controller

The CronTask controller creates a Task from a CronTask's template at each
scheduled time. It decides from the CronTask, the Tasks that carry its uid
label and the time. The manager performs each create and each delete.

| Read | Answers |
| --- | --- |
| [CronTask](../resources/cron-task.md) | What do I apply, and what does the status hold? |
| [State machine](#crontask-state-machine) | What happens at a scheduled time? |
| [Interface](#crontask-controller-interface) | What does the controller read, and what does the manager do? |
| [Task controller](task.md) | Who runs each Task that a CronTask creates? |

## One run

1. You apply a CronTask. Its first reconcile writes `nextScheduleTime` and
   `notBefore`. It creates no Task for a time before the CronTask existed.
2. The manager reconciles the CronTask again at `notBefore`, or at once
   when its spec changes.
3. The controller finds the latest scheduled time at or before now. If the
   controller already decided about that time, it does nothing.
4. Otherwise it decides about that time. It creates the Task, or it skips
   the time and says why in the `Skipped` condition.
5. It deletes the Concluded Tasks beyond `historyLimit`, oldest first.
6. It sets `notBefore` to the next scheduled time. While a create or a
   delete is in flight, the first attempt included, it sets `notBefore`
   to one minute later at most, so a create that never lands is retried
   long before the next scheduled time.

When the manager was down for several scheduled times, the controller
decides only about the latest one. It drops the earlier ones, as a
Kubernetes CronJob does.

## Implementation and evidence

| Responsibility | Source | Tests |
| --- | --- | --- |
| Pure decisions: the time, the policies, recovery, history | [CronTaskController](../../../crates/fleet-controllers/src/cron_task.rs) | [Controller passes](../../../crates/fleet-controllers/tests/cron_task_controller.rs), [CronTask E2E](../../../crates/fleet-cli/tests/e2e_cron_task.rs) |
| Resource, status and schedule math | [CronTask types](../../../crates/fleet-api/src/cron_task.rs), [validation](../../../crates/fleet-api/src/validate.rs) | Unit tests in `cron_task.rs`; [CronTask E2E](../../../crates/fleet-cli/tests/e2e_cron_task.rs) |
| Sweep, intent before effect, fenced create and retire, receipts | [Manager](../../../crates/fleet-manager/src/cron_tasks.rs) | [CronTask E2E](../../../crates/fleet-cli/tests/e2e_cron_task.rs) |
| The `crontask.fleet.dev/` labels, `retire_scheduled_task` | [Store](../../../crates/fleet-store/src/lib.rs) | [CronTask E2E](../../../crates/fleet-cli/tests/e2e_cron_task.rs) |
| Crate edges and the field manager | [architecture.toml](../../../crates/architecture.toml) | [Architecture suite](../../../crates/arch-rules/tests/architecture.rs) |

The E2E suite runs the manager on the file store with a test clock and the
fake runtime. It covers one Task per scheduled time, `Forbid`, `Allow`,
`Replace` and its wait for the old session, a missed deadline, `suspend`,
`historyLimit`, and recovery from an intent with no receipt: adoption, the
limits in force at the retry, and `NeedsAttention` for a create in doubt.
Through the `fleet` binary, it covers apply, get, list and delete, and the
refusals of a bad schedule, a bad template, template amendments and the
controller's labels.

## CronTask state machine

![CronTask state machine: at each scheduled time the controller skips the time when the CronTask is suspended, when the deadline passed, or when Forbid finds a Task that runs; with Replace it deletes the running Task first; then it writes the intent, the manager creates the Task, and the receipt is written](cron-task.svg)

First, the controller gives up each pending create that a fresh decision
would refuse now: the CronTask is suspended, the create's scheduled time is
past its deadline, or the policy is `Forbid` and another Task of this
CronTask runs. Such a create never blocks a later time. A retry always
obeys the spec in force, as a fresh decision does.

At a scheduled time `T`, the controller applies these rules in order:

1. `suspend` is true: no decision. The phase is `Suspended`.
2. `startingDeadlineSeconds` is set, and now is later than `T` plus the
   deadline: skip `T`. `Skipped` is True with reason `MissedDeadline`.
3. `concurrencyPolicy` is `Forbid`, and a Task of this CronTask is not
   Concluded, or a create is still pending: skip `T`. `Skipped` is True
   with reason `Forbidden`.
4. `concurrencyPolicy` is `Replace`: delete each Task of this CronTask that
   is not Concluded, and give up a create that is still pending.
5. Write the create's intent for `T`. `Skipped` is False with reason
   `Created`.

Then the controller makes each pending create, under the limits in force
at that moment:

- `suspend` is true: the create is given up.
- The deadline passed: the create is given up. `Skipped` is True with
  reason `MissedDeadline`.
- `concurrencyPolicy` is `Replace`, and another Task of this CronTask is
  not Concluded or is being deleted: the create waits. `Skipped` is False
  with reason `Replacing`. The controller looks again after one minute. A
  delete only marks the old Task, and the Task controller releases its
  session when the agent is at rest, or 5 minutes after the delete if the
  agent still reports work. The new Task is created only when the
  old one is gone from the store, so two agents never work at once.
- Otherwise the manager creates the Task.

A Task that is deleting does not count as running for `Forbid`.

### Conditions

| Type | Status | Reason | Meaning |
| --- | --- | --- | --- |
| `Scheduled` | True | `Scheduled` | The schedule parses, and the CronTask is not suspended. |
| `Scheduled` | False | `Suspended` | `spec.suspend` is true. |
| `Scheduled` | False | `InvalidSchedule` | The schedule does not parse. |
| `Skipped` | True | `MissedDeadline` | The last scheduled time came later than its deadline. |
| `Skipped` | True | `Forbidden` | A Task still ran at the last scheduled time. |
| `Skipped` | False | `Created` | The last scheduled time decided to create its Task. |
| `Skipped` | False | `Replacing` | The Task waits for the Task it replaces to be released. |
| `NeedsAttention` | True | `CreateUnknown` | The store could not say, through three attempts, whether a create happened. |
| `NeedsAttention` | False | `Resolved` | No create is in doubt any more. |

## CronTask controller interface

### Controller API

`CronTaskController` is in
[fleet-controllers](../../../crates/fleet-controllers/src/cron_task.rs). It
is pure.

| Item | Meaning |
| --- | --- |
| `CronTaskInput` | The CronTask, the Tasks whose `crontask.fleet.dev/uid` label is its uid, and `now`. |
| `CronTaskController::due(obj, now)` | True when the CronTask has no status, its spec changed, or `notBefore` came. |
| `CronTaskController::reconcile(input)` | Returns a `Plan`: the status, a `Create` for a new Task, and a `RetireScheduled` effect for each Task to delete. |

The schedule math is in
[`fleet_api::cron_task`](../../../crates/fleet-api/src/cron_task.rs). It
uses the `croner` crate, and it never reads the clock.

### Manager execution protocol

The manager reconciles the due CronTasks after the Tasks in each sweep. It
lists the Tasks once for all of them. When it cannot list the Tasks, it
reconciles no CronTask: an Unknown listing is never read as no Task.

For each due CronTask the manager does these steps:

1. It writes the planned status, compare-and-swap under the lease. The
   status holds the intent of each create and delete, by key.
2. It performs each delete with `Store::retire_scheduled_task`, fenced on
   the Task's uid. Then it writes the receipt under the key.
3. It performs the create with `Store::create_fenced`. Then it writes the
   receipt under the key.

A delete only marks the Task. Under `Replace` the controller plans the
replacement in a later pass, once the old Task is gone from the listing.

### Recovery

An intent with no receipt means that the manager stopped between the two
writes. The controller answers it from the Task listing:

- The Task exists and carries this CronTask's uid: the controller records
  the receipt `Created`, with `adopted` true.
- The Task does not exist: the create is pending again. The controller
  makes it under the same key and the same name, under the limits in force
  now (`suspend`, the deadline, `Replace`), and it counts as a running Task
  for `Forbid`.
- A create that the store refused three times is given up, with the
  receipt `Failed`.
- A delete keeps the permission it was decided with, `running`. A retry
  never widens it: a history delete stays Concluded-only, so it never
  removes a Task that a person reopened with `fleet amend` meanwhile. Only
  a newer `Replace` decision widens it. It is narrowed to Concluded-only
  while the spec no longer says `Replace`.
- A history delete that the store refused is decided again when the Task
  is Concluded again and still past `historyLimit`.
- A delete, like a create, brings `notBefore` within one minute, so one
  that never lands is retried long before the next scheduled time.
- A create whose outcome is Unknown is tried again every minute, with no
  limit: Fleet never reads Unknown as absence, so it never drops the create.
  While it waits it counts as a running Task, so under `Forbid` it holds
  later times. The record counts the attempts in doubt in `unknowns`: an
  attempt that read Unknown, or whose receipt never came. Only a Known
  outcome clears the count. After three attempts in doubt,
  `NeedsAttention` is True with reason `CreateUnknown`.

A create that finds a Task with the same name but without this uid is
refused. The controller never takes another CronTask's Task.

### Store port and write ownership

| Write | Who | Rule |
| --- | --- | --- |
| CronTask status | `cron-task-controller` | It owns every field. |
| A Task, created | `cron-task-controller` | Only through `create_fenced`, under the lease. |
| The `crontask.fleet.dev/name` and `-uid` labels | `cron-task-controller` | Set at create. `fleet apply` refuses them, and nobody changes them later. |
| A Task, deleted | `cron-task-controller` | Only through `retire_scheduled_task`: the Task's uid must match, the Task must carry this CronTask's uid, and it must be Concluded unless the policy is `Replace`. |
