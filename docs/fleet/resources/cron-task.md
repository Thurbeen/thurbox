# CronTask resource

A CronTask creates a [Task](task.md) at each time of a schedule. It is the
Kubernetes CronJob for Fleet: the CronJob makes a Job, and the CronTask
makes a Task. The [CronTask controller](../controllers/cron-task.md) does
the work.

## Example

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "CronTask",
  "metadata": {"name": "nightly-review"},
  "spec": {
    "schedule": "0 3 * * 1-5",
    "concurrencyPolicy": "Forbid",
    "startingDeadlineSeconds": 600,
    "historyLimit": 3,
    "template": {
      "metadata": {"labels": {"team": "review"}},
      "spec": {
        "harness": "claude-code",
        "host": "local",
        "brief": "Review what landed today and report what needs attention.",
        "workspace": ["git+https://github.com/example/app.git#main"],
        "dispatch": true
      }
    }
  },
  "status": {
    "phase": "Scheduled",
    "observedGeneration": 1,
    "lastScheduleTime": 1794798000000,
    "lastConsidered": 1794798000000,
    "nextScheduleTime": 1794884400000,
    "notBefore": 1794884400000,
    "active": ["nightly-review-29913300"],
    "effects": {
      "create/29913300": {
        "intent": {"action": "create", "at": 1794798004000, "generation": 1},
        "receipt": {"action": "create", "at": 1794798004000,
                    "outcome": {"known": {"type": "created", "uid": "u41-…", "adopted": false}}},
        "attempts": 1,
        "task": "nightly-review-29913300"
      }
    },
    "conditions": [
      {"type": "Scheduled", "status": "True", "reason": "Scheduled", "lastTransitionTime": 1794797000000, "observedGeneration": 1},
      {"type": "Skipped", "status": "False", "reason": "Created", "message": "Task/nightly-review-29913300", "lastTransitionTime": 1794798004000, "observedGeneration": 1}
    ]
  }
}
```

## Spec

You write the spec with `fleet apply`. You can change it at any time. A
change applies from the next scheduled time.

| Field | Required | Meaning |
| --- | --- | --- |
| `schedule` | yes | Five cron fields (minute, hour, day of month, month, day of week), or `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`. Fleet evaluates it in UTC. A seconds field is refused. |
| `template.spec` | yes | The spec of each Task. `fleet apply` validates it as a Task spec. |
| `template.metadata.labels` | no | Labels for each Task. The `crontask.fleet.dev/` and `gateway.fleet.dev/` labels are refused: only a controller sets them. |
| `concurrencyPolicy` | no | What happens at a scheduled time while an earlier Task of this CronTask is not Concluded. `Forbid` (the default) skips the time. `Allow` creates the Task. `Replace` deletes the earlier Task, then creates the new one. |
| `startingDeadlineSeconds` | no | A scheduled time that the controller sees more than this many seconds late is skipped. Without it, a late time is never skipped. |
| `suspend` | no | `true` stops new Tasks. The Tasks that run continue. |
| `historyLimit` | no | How many Concluded Tasks to keep, from 0 to 100. The default is 3. The controller deletes the older ones. |

`concurrencyPolicy` defaults to `Forbid`, not to `Allow` as in a CronJob.
A Task is an agent session that often works in one directory. Two sessions
at once are seldom what you want, so you ask for them explicitly.

The name of a CronTask is at most 240 characters, so that the name of each
Task fits.

## The Tasks it creates

The Task for the scheduled time `T` (Unix milliseconds) is named
`<cron-task>-<T / 60000>`. Each sweep computes the same name for the same
time, so a repeated decision cannot make a second Task.

Each Task has two labels that only the CronTask controller sets:

| Label | Value |
| --- | --- |
| `crontask.fleet.dev/name` | The CronTask's name. |
| `crontask.fleet.dev/uid` | The CronTask's `uid`. It tells this CronTask's Tasks from those of an earlier CronTask with the same name. |

The [Task controller](../controllers/task.md) runs each Task like any
other Task. The CronTask controller never writes a Task's status.

To list the Tasks of a CronTask, filter `fleet list tasks --json` on the
`crontask.fleet.dev/name` label.

## Status

The CronTask controller writes all of the status.

| Field | Meaning |
| --- | --- |
| `phase` | `Scheduled`, or `Suspended` while `spec.suspend` is true. |
| `observedGeneration` | The spec generation of the last reconcile. |
| `lastScheduleTime` | The scheduled time of the last Task created. |
| `lastConsidered` | The last scheduled time that the controller decided about: created, skipped or forbidden. |
| `nextScheduleTime` | The next scheduled time. |
| `notBefore` | When the manager reconciles the CronTask again. |
| `active` | The names of this CronTask's Tasks that are not Concluded. |
| `effects` | Each create and each delete by key: its `intent`, written before the effect, and its `receipt`, written after. `task` names the Task. The controller keeps the newest 20 settled records. |
| `conditions` | `Scheduled` and `Skipped`. See [conditions](../controllers/cron-task.md#conditions). |

## Metadata and ownership

`kind` is `CronTask`. It has no finalizer. A delete removes the CronTask at
once.

The Tasks of a deleted CronTask stay. A Task holds a session and maybe work
that is not merged, so Fleet does not delete it for you. Delete the Tasks
with `fleet delete task NAME`.

## Commands

```bash
fleet apply -f nightly-review.json
fleet get crontask nightly-review
fleet list crontasks
fleet status crontask nightly-review
fleet delete crontask nightly-review
```
