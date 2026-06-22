#!/usr/bin/env bash
# upsert.sh — reconcile a normalized issue list into the thurbox task list.
#
# Reads a JSON array on stdin (the output of fetch.sh):
#   [{ "external_id": "..", "title": "..", "status": "todo|in_progress|done",
#      "url": "..", "description": ".." }]
# Dedups by (source, external_id): creates tasks that don't exist yet, and edits
# the ones whose title / url / open-vs-closed state changed. Provider-neutral —
# every task-integration extension ships an identical copy.
#
# Status rule (pull direction): the tracker is authoritative for the title, url,
# and the **open-vs-done** axis. The finer todo↔in_progress split is preserved
# locally — when both sides are "open" the thurbox status is left untouched, so a
# user marking a synced task `in_progress` is never clobbered back to `todo` on
# the next pull. Run push-status.sh BEFORE this (push-then-pull) so any local
# done/reopen intent is already reflected on the tracker.
#
# Usage:  fetch.sh "<query>" | upsert.sh --source github

set -euo pipefail

SOURCE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --source) SOURCE="${2:?--source needs a value}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$SOURCE" ] || { echo "usage: upsert.sh --source <tag>" >&2; exit 2; }

command -v thurbox-cli >/dev/null 2>&1 || { echo "thurbox-cli not found" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not found" >&2; exit 3; }

incoming="$(cat)"; [ -n "$incoming" ] || incoming='[]'

# Snapshot existing tasks for this source once (so we don't re-list per item).
existing="$(thurbox-cli task list --json 2>/dev/null \
  | jq --arg s "$SOURCE" '[ .[] | select(.source == $s) ]')"
[ -n "$existing" ] || existing='[]'

is_done() { [ "$1" = "done" ]; }

created=0; updated=0; unchanged=0
while IFS= read -r item; do
  [ -n "$item" ] || continue
  eid="$(jq -r '.external_id' <<<"$item")"
  title="$(jq -r '.title' <<<"$item")"
  status="$(jq -r '.status // "todo"' <<<"$item")"
  url="$(jq -r '.url // ""' <<<"$item")"
  desc="$(jq -r '.description // ""' <<<"$item")"

  match="$(jq -c --arg id "$eid" 'map(select(.external_id == $id)) | .[0] // empty' <<<"$existing")"

  if [ -z "$match" ]; then
    # New issue → create. Description is optional (omit the flag when blank).
    if [ -n "$desc" ]; then
      thurbox-cli task create --title "$title" --status "$status" \
        --source "$SOURCE" --external-id "$eid" --external-url "$url" \
        --description "$desc" >/dev/null
    else
      thurbox-cli task create --title "$title" --status "$status" \
        --source "$SOURCE" --external-id "$eid" --external-url "$url" >/dev/null
    fi
    created=$((created + 1))
    continue
  fi

  id="$(jq -r '.id' <<<"$match")"
  cur_title="$(jq -r '.title' <<<"$match")"
  cur_status="$(jq -r '.status' <<<"$match")"
  cur_url="$(jq -r '.external_url // ""' <<<"$match")"

  # Open-vs-done axis is authoritative; preserve local todo↔in_progress.
  new_status="$cur_status"
  if is_done "$status" && ! is_done "$cur_status"; then
    new_status="done"
  elif ! is_done "$status" && is_done "$cur_status"; then
    new_status="todo" # remote reopened it; drop back to todo
  fi

  if [ "$cur_title" != "$title" ] || [ "$cur_status" != "$new_status" ] \
     || [ "$cur_url" != "$url" ]; then
    thurbox-cli task edit "$id" --title "$title" --status "$new_status" \
      --external-url "$url" >/dev/null
    updated=$((updated + 1))
  else
    unchanged=$((unchanged + 1))
  fi
done < <(jq -c '.[]' <<<"$incoming")

echo "upsert[$SOURCE]: created=$created updated=$updated unchanged=$unchanged"
