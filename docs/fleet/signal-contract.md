# Signal contract for agent vendors

This page is for the vendor of a coding agent. It tells you how your agent
reports its state to Fleet. Fleet then shows the state in thurbox, proves
prompt delivery, and resumes the agent on its conversation.

Your agent needs no Fleet library. It runs one command from its hooks.

```text
 Fleet starts your agent             your agent's hook
 ───────────────────────             ─────────────────
 env FLEET_SIGNAL_FILE=<file>  ───▶  FLEET_SIGNAL_FILE set?
 env FLEET_HARNESS=<harness>           │ no ──▶ do nothing
                                       │ yes
                                       ▼
                               fleet signal <state>  ◀── hook JSON on stdin
                                       │
                                       ▼
                               one JSON line in <file> ──▶ Fleet reads it
```

## The rule

1. Read `FLEET_SIGNAL_FILE` from the environment.
2. If it is unset or empty, do nothing. Fleet did not start this session.
3. If it is set, run `fleet signal <state>`. Write the hook's JSON payload
   to its standard input.
4. Ignore the result. Do not fail the agent's turn when `fleet` is missing
   or fails. For example, in a POSIX shell:
   `fleet signal idle 2>/dev/null || true`.

`fleet signal` also does nothing when `FLEET_SIGNAL_FILE` is unset. The check
in step 2 only saves one process.

## States

| State | Run it when | Fleet shows |
| --- | --- | --- |
| `prompt` | The agent receives a prompt, before the turn starts. | `working` |
| `working` | The agent works on a turn, for example before a tool runs. | `working` |
| `blocked` | The agent waits for a person to approve something. | `attention` |
| `idle` | The turn ends and the agent waits for input. Also at session start. | `idle` |

Use these rules:

- Send `idle` at session start. Fleet then knows the agent is ready for its
  brief.
- Send `blocked` only when a person must act. Do not send it when the agent
  only waits for its next prompt.
- Until the agent sends a state, Fleet shows `unknown`. Fleet never guesses
  `idle`.

## Payload

The payload is the hook's JSON object. Fleet reads two kinds of field from
it. Each harness profile names the fields in its `[payload]` table
([harness profiles](controllers/task.md#harness-profiles)).

| Field kind | Default names | Value | Fleet uses it to |
| --- | --- | --- | --- |
| prompt | `prompt`, `input-messages` | a string, or a list of strings or of parts with a `text` string | prove that a prompt reached the agent |
| conversation | `session_id`, `thread-id` | a string | resume the agent on its conversation |

- Fleet stores a prompt only as a digest. It never stores the prompt text.
- Fleet reads the first conversation field that is a string. An empty
  string is no conversation.
- Nonempty `background_tasks` or `session_crons` lists turn `idle` into
  `working`: the turn left work running.
- `notification_type = "idle_prompt"` turns `blocked` into `idle`: the agent
  only waits for its next prompt.
- These two state rules apply to every harness. Fleet ignores every other
  field.
- A payload that is not JSON gives no prompt and no conversation. The state
  still counts.

`fleet signal` also takes the payload as its last argument, in place of
stdin.

## Which fields Fleet reads

Fleet sets `FLEET_HARNESS` to the session's harness name at spawn, and
`FLEET_HARNESS_DIR` to the directory it read the user's profiles from.
`fleet signal` reads the payload with the `[payload]` fields of that
harness's profile:

1. `fleet signal --harness <name>` names the harness. It overrides
   `FLEET_HARNESS`.
2. Else `FLEET_HARNESS` names the harness.
3. A harness with no profile, or no name, uses the default names.

`fleet signal` reads the user's profiles from `FLEET_HARNESS_DIR`. Without
it, it reads them beside the config file that this `fleet` finds
(`--config`, else `FLEET_CONFIG`, else the per-user config). A user profile
that `fleet signal` cannot read does not fail the hook. Fleet skips that one
file, and the bundled profile or the default names apply.

## Limits

- One `fleet signal` run writes one line of at most 4 KiB.
- Fleet reads the signal file only for a session on its own machine. For a
  session on another host, the state stays Unknown.

## Examples

Fleet's own hook files for pi, opencode, kilo, omp and qwen follow this contract
([harness profiles](controllers/task.md#harness-profiles)).

A hook that reports the end of a turn, with the conversation `s1`:

```sh
[ -n "$FLEET_SIGNAL_FILE" ] || exit 0
printf '%s' '{"session_id":"s1"}' | fleet signal idle 2>/dev/null || true
```
