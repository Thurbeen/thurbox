#!/usr/bin/env bash
# flow-snapshot.sh — one-call compact view of the flow agent's world:
# the task backlog grouped by status + the live flow / task-* sessions.
# Keeps the triager to a single shell call per mode.

set -euo pipefail

echo "## tasks"
if TASKS="$(thurbox-cli task list 2>/dev/null)"; then
  printf '%s' "$TASKS" | jq -r '
    group_by(.status) | .[] |
    "### \(.[0].status) (\(length))",
    (.[] | "  #\(.id) \(.title)"
      + (if .action.kind == "spawn" then " [spawn:\(.action.agent // "default")]"
         elif .action.kind == "send" then " [send]"
         else " [todo-only]" end)
      + (if .description then
           ((.description | split("\n")[0]) as $p |
            if $p | startswith("priority:") then " {\($p)}" else "" end)
         else "" end))
  ' 2>/dev/null || printf '%s\n' "$TASKS"
else
  echo "  (thurbox-cli task list failed)"
fi

echo
echo "## sessions (flow / task-*)"
if SESSIONS="$(thurbox-cli session list 2>/dev/null)"; then
  printf '%s' "$SESSIONS" | jq -r '
    .[] | select(.name == "flow" or (.name | startswith("task-"))) |
    "  \(.name)  \(.id)  agent=\(.agent)  cwd=\(.cwd)"
  ' 2>/dev/null || true
else
  echo "  (thurbox-cli session list failed)"
fi
