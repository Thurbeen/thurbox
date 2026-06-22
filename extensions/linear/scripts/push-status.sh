#!/usr/bin/env bash
# push-status.sh — push thurbox task status back to Linear (the bidirectional
# half). For every thurbox task with source=linear whose tracker row opts into
# push_back, reconcile the Linear issue's open/done state with the task:
#   task done              -> issue moved to a completed state
#   task todo|in_progress  -> issue moved back to an unstarted (open) state
# Only the open-vs-done axis is pushed.
#
# external_id is the issue identifier (e.g. "ENG-7"); the team key is the part
# before the `-`, which gates which rows are eligible (a tracker row's query is
# a team key).
#
# Run this BEFORE fetch.sh|upsert.sh (push-then-pull) so the tracker reflects
# local intent before the pull reads it back.
#
# Usage:  push-status.sh        # reads ../trackers.md, acts on push_back=yes rows
#         push-status.sh --dry-run

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
TRACKERS_MD="$HOME_DIR/trackers.md"
SOURCE="linear"
DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

command -v curl >/dev/null 2>&1 || { echo "curl not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

: "${LINEAR_API_KEY:?LINEAR_API_KEY is unset (set your Linear personal API key)}"

# Call the Linear GraphQL API. The personal API key is sent VERBATIM in the
# Authorization header (Linear keys are NOT prefixed with "Bearer").
linear_gql() { # <graphql-query-json-payload on $1>
  curl -fsSL -X POST https://api.linear.app/graphql \
    -H "Authorization: $LINEAR_API_KEY" -H "Content-Type: application/json" \
    --data "$1"
}

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# Collect the set of team keys whose tracker row has push_back=yes (the query is
# a Linear team key).
declare -A ENABLED_TEAM=()
if [ -f "$TRACKERS_MD" ]; then
  while IFS='|' read -r _ _name query push_back _; do
    query="$(trim "$query")"; push_back="$(trim "$push_back")"
    [ -n "$query" ] || continue
    case "$(printf '%s' "$push_back" | tr '[:upper:]' '[:lower:]')" in
      yes|y|true|1) ENABLED_TEAM["$query"]=1 ;;
    esac
  done < <(grep '^[[:space:]]*|' "$TRACKERS_MD" \
            | grep -viE '^\s*\|\s*name\s*\|' \
            | grep -vE '^\s*\|[[:space:]]*-')
fi

if [ "${#ENABLED_TEAM[@]}" -eq 0 ]; then
  echo "push-status[$SOURCE]: no push_back=yes trackers — nothing to push"
  exit 0
fi

pushed=0; skipped=0
while IFS= read -r task; do
  [ -n "$task" ] || continue
  eid="$(jq -r '.external_id // ""' <<<"$task")"
  status="$(jq -r '.status' <<<"$task")"
  team="${eid%%-*}"
  [ -n "$eid" ] && [ -n "$team" ] && [ "$team" != "$eid" ] || { skipped=$((skipped+1)); continue; }
  [ -n "${ENABLED_TEAM[$team]:-}" ] || { skipped=$((skipped+1)); continue; }

  # Fetch the issue's current state type + its team's workflow states.
  # shellcheck disable=SC2016  # $id is a GraphQL variable, not a shell expansion.
  q='query($id: String!) { issue(id: $id) { id state { type } team { states { nodes { id type } } } } }'
  payload="$(jq -n --arg q "$q" --arg id "$eid" '{query: $q, variables: {id: $id}}')"
  resp="$(linear_gql "$payload" 2>/dev/null || true)"
  cur_type="$(printf '%s' "$resp" | jq -r '.data.issue.state.type // ""' 2>/dev/null || echo "")"
  [ -n "$cur_type" ] || { echo "  ! $eid: cannot read state"; skipped=$((skipped+1)); continue; }

  desired="open"; [ "$status" = "done" ] && desired="done"

  target_id=""
  if [ "$desired" = "done" ] && [ "$cur_type" != "completed" ] && [ "$cur_type" != "canceled" ]; then
    # Pick the team's first completed state.
    target_id="$(printf '%s' "$resp" | jq -r \
      'first(.data.issue.team.states.nodes[] | select(.type == "completed") | .id) // ""')"
  elif [ "$desired" = "open" ] && { [ "$cur_type" = "completed" ] || [ "$cur_type" = "canceled" ]; }; then
    # Pick the team's first unstarted state, falling back to backlog.
    target_id="$(printf '%s' "$resp" | jq -r \
      '(first(.data.issue.team.states.nodes[] | select(.type == "unstarted") | .id))
       // (first(.data.issue.team.states.nodes[] | select(.type == "backlog") | .id)) // ""')"
  else
    skipped=$((skipped+1)); continue
  fi

  if [ -z "$target_id" ]; then
    echo "  ! $eid: no $desired workflow state on team $team"; skipped=$((skipped+1)); continue
  fi

  verb="reopen"; [ "$desired" = "done" ] && verb="close"
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  would $verb $eid"
    pushed=$((pushed+1))
    continue
  fi

  # Linear's issueUpdate accepts the identifier as `id`.
  # shellcheck disable=SC2016  # $id/$sid are GraphQL variables, not shell expansions.
  m='mutation($id: String!, $sid: String!) { issueUpdate(id: $id, input: { stateId: $sid }) { success } }'
  mpayload="$(jq -n --arg q "$m" --arg id "$eid" --arg sid "$target_id" \
    '{query: $q, variables: {id: $id, sid: $sid}}')"
  past="reopened"; [ "$desired" = "done" ] && past="closed"
  ok="$(linear_gql "$mpayload" 2>/dev/null | jq -r '.data.issueUpdate.success // false' 2>/dev/null || echo false)"
  if [ "$ok" = "true" ]; then
    echo "  $past $eid"
    pushed=$((pushed+1))
  else
    echo "  ! $eid: issueUpdate failed"
    skipped=$((skipped+1))
  fi
done < <(thurbox-cli task list --json 2>/dev/null \
          | jq -c --arg s "$SOURCE" '.[] | select(.source == $s)')

echo "push-status[$SOURCE]: pushed=$pushed skipped=$skipped"
