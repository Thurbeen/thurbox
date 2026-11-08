---
name: status-digest
description: One line per Task of everything Mission Control tracks, read from Fleet, and the labels that record order, limits and pending decisions. Load for any status question, at session start and after a clear.
license: MIT
---

# Status digest

Fleet holds Mission Control's state. Read it with this one command, which needs
`jq`, and work from its lines. Do not dump raw JSON or keep a copy elsewhere.

```sh
fleet list task --json | jq -r '.[] | select(.metadata.name != "vega") | (.status // {}) as $s | [.metadata.name, ($s.phase // "-"), (if .spec.dispatch then empty else "held" end), ((.metadata.labels // {}) | to_entries[] | select(.key | startswith("vega.fleet.dev/")) | "\(.key | ltrimstr("vega.fleet.dev/")):\(.value)"), (($s.declared // [])[] | select(.kind == "ChangeRequest") | "cr:\(.url)"), ([($s.declared // [])[] | select(.kind == "Question") | .id] - (($s.answered // {}) | keys) | .[] | "ask:\(.)"), ($s.result // empty | "result:\(.outcome)"), (($s.conditions // [])[] | select(.type == "NeedsAttention" and .status == "True") | "attention:\(.reason)")] | join("  ")'
```

| Field | Meaning |
| --- | --- |
| `held` | `dispatch` is false: queued behind `after`, or waiting on the user. |
| `request:` | The request, in the user's words. |
| `after:` | Dispatch this Task when the named one concludes. |
| `limits:` | The user's authorization limits, such as `draft-only no-merge`. |
| `needs:` | A decision waiting on the user. |
| `ask:ID` | A worker Question not yet answered: `fleet send NAME --answers ID TEXT`. |
| `cr:URL` | A change the Task declared, so the Task that owns it. |
| `result:` | The worker's claimed outcome; verify it with task-status. |
| `attention:` | A `NeedsAttention` reason; inspect with task-status. |

A user answer for a held Task goes into its labels or a new Task, never only
into this conversation. Labels change no spec or brief. Set or replace one:

```sh
fleet get task NAME | jq '.metadata.labels["vega.fleet.dev/needs"] = "choose a base branch" | del(.status)' | fleet apply -f -
```

Release a held Task only when its `after` Task concluded with a verified
`shipped` result and nothing is needed; any other outcome goes to the user:

```sh
fleet get task NAME | jq '.spec.dispatch = true | .metadata.labels |= del(.["vega.fleet.dev/after"], .["vega.fleet.dev/needs"]) | del(.status)' | fleet apply -f -
```

A `Concluded` line with no `ask:` or `attention:` is finished; report it once.
Delete a Task only when the user authorizes cleanup, as task-status explains.
