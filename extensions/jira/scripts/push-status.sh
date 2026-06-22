#!/usr/bin/env bash
# push-status.sh — push thurbox task status back to Jira (the bidirectional
# half). For every thurbox task with source=jira, reconcile the Jira issue's
# open/closed state with the task:
#   task done              -> issue moved to a Done-category status
#   task todo|in_progress  -> issue moved to a non-Done status (reopened)
# Only the open-vs-done axis is pushed (Jira's finer status is left alone once
# it's in the right category). external_id is the issue key (e.g. "ENG-7").
#
# push_back gating: Jira issue keys don't cleanly map a task back to a specific
# JQL tracker row, so push-back is gated on "at least ONE trackers.md row has
# push_back enabled" — when so, this acts on ALL source=jira tasks. Leave every
# row `no` to disable push entirely.
#
# Run this BEFORE fetch.sh|upsert.sh (push-then-pull) so the tracker reflects
# local intent before the pull reads it back.
#
# Usage:  push-status.sh        # reads ../trackers.md; runs only if any row opts in
#         push-status.sh --dry-run
#
# Auth: JIRA_BASE_URL, JIRA_EMAIL, JIRA_API_TOKEN (HTTP Basic, REST API v3).

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
TRACKERS_MD="$HOME_DIR/trackers.md"
SOURCE="jira"
DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

command -v curl >/dev/null 2>&1 || { echo "curl not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

: "${JIRA_BASE_URL:?set JIRA_BASE_URL (e.g. https://your.atlassian.net)}"
: "${JIRA_EMAIL:?set JIRA_EMAIL (your Atlassian account email)}"
: "${JIRA_API_TOKEN:?set JIRA_API_TOKEN (an Atlassian API token)}"

jira_get() { curl -fsSL -u "$JIRA_EMAIL:$JIRA_API_TOKEN" -H 'Accept: application/json' "$@"; }
jira_post() {
  curl -fsSL -u "$JIRA_EMAIL:$JIRA_API_TOKEN" \
    -X POST -H 'Content-Type: application/json' "$@"
}

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# Determine whether ANY tracker row opts into push_back (the gate — Jira keys
# can't be tied back to a specific row, so it's all-or-nothing).
push_enabled=0
if [ -f "$TRACKERS_MD" ]; then
  while IFS='|' read -r _ _name _query push_back _; do
    push_back="$(trim "$push_back")"
    case "$(printf '%s' "$push_back" | tr '[:upper:]' '[:lower:]')" in
      yes|y|true|1) push_enabled=1 ;;
    esac
  done < <(grep '^[[:space:]]*|' "$TRACKERS_MD" \
            | grep -viE '^\s*\|\s*name\s*\|' \
            | grep -vE '^\s*\|[[:space:]]*-')
fi

if [ "$push_enabled" -eq 0 ]; then
  echo "push-status[$SOURCE]: no push_back=yes trackers — nothing to push"
  exit 0
fi

# Pick a transition id moving the issue INTO ($want=done) or OUT OF ($want=open)
# the Done category. For reopen, prefer a transition landing on the "new"
# category. Prints the chosen transition id, or nothing if none qualifies.
pick_transition() {
  key="$1"; want="$2"
  trans="$(jira_get "$JIRA_BASE_URL/rest/api/3/issue/$key/transitions" 2>/dev/null || echo '')"
  [ -n "$trans" ] || { printf ''; return; }
  if [ "$want" = "done" ]; then
    printf '%s' "$trans" | jq -r '
      [ .transitions[] | select(.to.statusCategory.key == "done") ] | .[0].id // empty'
  else
    printf '%s' "$trans" | jq -r '
      ([ .transitions[] | select(.to.statusCategory.key == "new") ] | .[0].id)
      // ([ .transitions[] | select(.to.statusCategory.key != "done") ] | .[0].id)
      // empty'
  fi
}

pushed=0; skipped=0
while IFS= read -r task; do
  [ -n "$task" ] || continue
  key="$(jq -r '.external_id // ""' <<<"$task")"
  status="$(jq -r '.status' <<<"$task")"
  [ -n "$key" ] || { skipped=$((skipped+1)); continue; }

  want="open"; [ "$status" = "done" ] && want="done"

  cur="$(jira_get "$JIRA_BASE_URL/rest/api/3/issue/$key?fields=status" 2>/dev/null \
         | jq -r '.fields.status.statusCategory.key' 2>/dev/null || echo "")"
  [ -n "$cur" ] && [ "$cur" != "null" ] || { echo "  ! $key: cannot read status"; skipped=$((skipped+1)); continue; }

  # Only transition when the current category disagrees with desired.
  if { [ "$want" = "done" ] && [ "$cur" = "done" ]; } \
     || { [ "$want" = "open" ] && [ "$cur" != "done" ]; }; then
    skipped=$((skipped+1)); continue
  fi

  tid="$(pick_transition "$key" "$want")"
  if [ -z "$tid" ]; then
    echo "  ! $key: no transition to ${want} category (workflow lacks it) — skipped"
    skipped=$((skipped+1)); continue
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  would transition $key -> $want (transition $tid)"
    pushed=$((pushed+1)); continue
  fi

  body="$(jq -n --arg id "$tid" '{transition:{id:$id}}')"
  if jira_post --data "$body" "$JIRA_BASE_URL/rest/api/3/issue/$key/transitions" >/dev/null 2>&1; then
    echo "  transitioned $key -> $want"
    pushed=$((pushed+1))
  else
    echo "  ! $key: transition POST failed — skipped"
    skipped=$((skipped+1))
  fi
done < <(thurbox-cli task list --json 2>/dev/null \
          | jq -c --arg s "$SOURCE" '.[] | select(.source == $s)')

echo "push-status[$SOURCE]: pushed=$pushed skipped=$skipped"
