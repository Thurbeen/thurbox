#!/usr/bin/env bash
# push-status.sh — push thurbox task status back to GitLab (the bidirectional
# half). For every thurbox task with source=gitlab whose tracker row opts into
# push_back, reconcile the GitLab issue's open/closed state with the task:
#   task done              -> issue closed
#   task todo|in_progress  -> issue open (reopened if currently closed)
# Only the open-vs-closed axis is pushed (GitLab issues have no in_progress).
# external_id is "group/project#<iid>"; the project gates which rows are eligible.
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
SOURCE="gitlab"
DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

command -v glab >/dev/null 2>&1 || { echo "glab (GitLab CLI) not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# Collect the set of projects whose tracker row has push_back=yes (the first word
# of each opted-in query).
declare -A ENABLED_PROJECT=()
if [ -f "$TRACKERS_MD" ]; then
  while IFS='|' read -r _ _name query push_back _; do
    query="$(trim "$query")"; push_back="$(trim "$push_back")"
    [ -n "$query" ] || continue
    case "$(printf '%s' "$push_back" | tr '[:upper:]' '[:lower:]')" in
      yes|y|true|1) ENABLED_PROJECT["${query%% *}"]=1 ;;
    esac
  done < <(grep '^[[:space:]]*|' "$TRACKERS_MD" \
            | grep -viE '^\s*\|\s*name\s*\|' \
            | grep -vE '^\s*\|[[:space:]]*-')
fi

if [ "${#ENABLED_PROJECT[@]}" -eq 0 ]; then
  echo "push-status[$SOURCE]: no push_back=yes trackers — nothing to push"
  exit 0
fi

pushed=0; skipped=0
while IFS= read -r task; do
  [ -n "$task" ] || continue
  eid="$(jq -r '.external_id // ""' <<<"$task")"
  status="$(jq -r '.status' <<<"$task")"
  project="${eid%%#*}"; iid="${eid##*#}"
  [ -n "$project" ] && [ -n "$iid" ] && [ "$project" != "$iid" ] || { skipped=$((skipped+1)); continue; }
  [ -n "${ENABLED_PROJECT[$project]:-}" ] || { skipped=$((skipped+1)); continue; }

  want_closed=0; [ "$status" = "done" ] && want_closed=1
  # NB: `glab issue view` uses -F/--output (text|json); `glab issue list` uses
  # -O/--output. The two subcommands disagree on the flag letter — keep -F here.
  cur="$(glab issue view "$iid" -R "$project" -F json 2>/dev/null \
         | jq -r '.state' 2>/dev/null || echo "")"
  [ -n "$cur" ] || { echo "  ! $project#$iid: cannot read state"; skipped=$((skipped+1)); continue; }

  if [ "$want_closed" -eq 1 ] && [ "$cur" != "closed" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then echo "  would close $project#$iid"; else
      glab issue close "$iid" -R "$project" >/dev/null && echo "  closed $project#$iid"
    fi
    pushed=$((pushed+1))
  elif [ "$want_closed" -eq 0 ] && [ "$cur" = "closed" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then echo "  would reopen $project#$iid"; else
      glab issue reopen "$iid" -R "$project" >/dev/null && echo "  reopened $project#$iid"
    fi
    pushed=$((pushed+1))
  else
    skipped=$((skipped+1))
  fi
done < <(thurbox-cli task list --json 2>/dev/null \
          | jq -c --arg s "$SOURCE" '.[] | select(.source == $s)')

echo "push-status[$SOURCE]: pushed=$pushed skipped=$skipped"
