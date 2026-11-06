# Claude Code `Stop` hook payloads

Fixtures for the `background_tasks` / `session_crons` fields of Claude Code's
`Stop` hook input, read by `session signal --stop-payload` (issue: session
activity). Their shape follows the payload builder in Claude Code 2.1.318, field
for field:

- `background_tasks[]`: one entry per in-flight task (running, pending or
  backgrounded; Claude's own `post_turn_memory` task is left out) with `id`,
  `type` (`shell`, `subagent`, `monitor`, `workflow`, `teammate`,
  `cloud session`, `MCP task`, …), `status` and `description`, plus a
  per-type extra (`command` for a shell, `agent_type` for a subagent,
  `server`/`tool` for a monitor or MCP task, `name` for a workflow).
- `session_crons[]`: `{id, schedule, recurring, prompt}`, from CronCreate,
  ScheduleWakeup and `/loop`.

Both are serialised compactly by `JSON.stringify`, which the remote rewrite of
the `Stop` hook relies on (`"background_tasks":[{`).

They were not captured from a live session: a headless `claude -p` could not
authenticate on the machine this was written on. Replace them with a captured
payload when one is to hand.
