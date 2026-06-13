#!/usr/bin/env bash
# flow-summary.sh — at-a-glance table of the flow + worker sessions, joined to
# their tasks (status / age / title). Printed at the top of every TICK so the
# user can see the whole board without parsing verbose worker output.
#
# Each row is one live `flow` / `task-*` session. `task-<id>-…` sessions are
# joined to task #<id> for status, age (from created_at) and title; a session
# whose task is gone shows "orphan", and tasks whose worker session is missing
# are listed under a "detached" footer line (so stale work isn't silently lost).
#
# Columns: ID  SESSION  AGENT  STATUS  AGE  TITLE
# Status glyphs: ◆ monitor (flow) · ● running · ○ queued · ✔ done · ⚠ orphan
#
# No arguments. Reads `thurbox-cli session list` + `task list`; degrades to a
# one-line notice if either call fails or there are no sessions to show.

set -euo pipefail

SESSIONS="$(thurbox-cli session list 2>/dev/null || echo '[]')"
TASKS="$(thurbox-cli task list 2>/dev/null || echo '[]')"

ROWS="$(printf '%s' "$SESSIONS" | jq -r --argjson tasks "$TASKS" '
  # tasks indexed by id (string keys)
  ($tasks | map({ key: (.id|tostring), value: . }) | from_entries) as $byid |

  def trunc($n): if (.|length) > $n then (.[0:$n-1] + "…") else . end;
  def humanage($ms):
    ((now - ($ms / 1000)) | floor) as $s |
    if   $s < 0       then "—"
    elif $s < 90      then "\($s)s"
    elif $s < 5400    then "\(($s / 60)    | floor)m"
    elif $s < 172800  then "\(($s / 3600)  | floor)h"
    else                   "\(($s / 86400) | floor)d" end;
  def statusglyph($st):
    if   $st == "done"        then "✔ done"
    elif $st == "in_progress" then "● running"
    elif $st == "todo"        then "○ queued"
    else $st end;

  [ .[]
    | select(.name == "flow" or (.name | startswith("task-")))
    # extract <id> from a task-<id>-… session name (null for the flow session)
    | (.name | [scan("^task-([0-9]+)-")] | (first // [])[0]) as $tid
    | (if $tid then $byid[$tid] else null end) as $task
    | {
        id:      (if $tid then "#\($tid)" else "—" end),
        session: (.name | trunc(22)),
        agent:   (.agent // "—"),
        status:  (
          if .name == "flow"      then "◆ monitor"
          elif $task             then statusglyph($task.status)
          else "⚠ orphan" end),
        age:     (if $task then humanage($task.created_at) else "—" end),
        title:   (if .name == "flow" then "(triage monitor)"
                  elif $task         then ($task.title | trunc(40))
                  else "(no task)" end),
      }
  ]
  # flow first, then worker sessions by id
  | sort_by((.id == "—" | not), .id)
  | select(length > 0)
  | (["ID","SESSION","AGENT","STATUS","AGE","TITLE"]),
    (.[] | [.id, .session, .agent, .status, .age, .title])
  | @tsv
' 2>/dev/null || true)"

if [[ -z "$ROWS" ]]; then
  echo "## flow board — (no flow/worker sessions)"
  exit 0
fi

echo "## flow board"
if command -v column >/dev/null 2>&1; then
  printf '%s\n' "$ROWS" | column -t -s $'\t'
else
  printf '%s\n' "$ROWS" | tr '\t' ' '
fi

# Footer: tasks that are in_progress but whose worker session is gone (detached)
DETACHED="$(printf '%s' "$TASKS" | jq -r --argjson sessions "$SESSIONS" '
  ($sessions | map(.name)) as $names |
  .[]
  | select(.status == "in_progress")
  | select( [ $names[] | startswith("task-\(.id|tostring)-") ] | any | not )
  | "  #\(.id) \(.title)"
' 2>/dev/null || true)"

if [[ -n "$DETACHED" ]]; then
  echo
  echo "## detached (in_progress, no worker session)"
  printf '%s\n' "$DETACHED"
fi
