#!/usr/bin/env bash
# flow-snapshot.sh — one-call compact view of the flow agent's world:
# the task backlog grouped by status + the live flow / worker sessions.
# Keeps the triager to a single shell call per mode.

set -euo pipefail

echo "## tasks"
# `--json` is explicit because `jq` parses this below: thurbox-cli answers TOON
# down a pipe, and the fallback here dumps the raw answer, so the board would
# render as TOON rather than fail.
if TASKS="$(thurbox-cli --json task list 2>/dev/null)"; then
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
echo "## sessions (flow / workers)"
# Worker sessions are named "<title> · #<id>" (current) or "task-<id>[-…]"
# (legacy) — mirror Task::matches_spawn_session. The derived #<id> is printed
# first purely for the **human board** (it tells you which task a worker is on).
# Routing no longer relies on it: ANSWER replies by message id
# (`message reply <id>`), and thurbox resolves the sender — flow never maps a
# task id to a session uuid.
if SESSIONS="$(thurbox-cli session list --json 2>/dev/null)"; then
  printf '%s' "$SESSIONS" | jq -r '
    .[]
    # extract <id> from "<title> · #<id>" (current) or "task-<id>[-…]" (legacy);
    # null for the flow session and any non-worker session
    | (.name
       | (([scan(" · #([0-9]+)$")] | (first // [])[0])
          // ([scan("^task-([0-9]+)(?:-|$)")] | (first // [])[0]))) as $tid
    | select(.name == "flow" or $tid != null)
    | (if $tid then "#\($tid)" else "—" end) as $idtag
    | "  \($idtag)  \(.name)  \(.id)  agent=\(.agent)  cwd=\(.cwd)"
  ' 2>/dev/null || true
else
  echo "  (thurbox-cli session list failed)"
fi
