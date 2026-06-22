#!/usr/bin/env bash
# fetch.sh — list a GitHub repo's issues as a normalized JSON array for upsert.sh.
#
# Takes ONE argument: the tracker row's `query`, whose first word is the repo
# (`owner/repo`) and whose remaining words are extra `gh issue list` flags:
#   fetch.sh "owner/repo --state open --assignee @me --label bug"
#
# Output element (the contract upsert.sh consumes):
#   { "external_id": "owner/repo#<number>",   # unique under source=github
#     "title": "..", "status": "todo|in_progress|done",
#     "url": "..", "description": ".." }
# Status mapping: CLOSED → done; OPEN with an assignee → in_progress; OPEN → todo.

set -euo pipefail

command -v gh >/dev/null 2>&1 || { echo "gh (GitHub CLI) not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

QUERY="${1:?usage: fetch.sh \"<owner/repo> [gh issue list flags]\"}"
# Word-split the query: repo + passthrough flags.
read -ra PARTS <<<"$QUERY"
REPO="${PARTS[0]:?query must start with owner/repo}"
FLAGS=("${PARTS[@]:1}")
# Default to open issues when the row gave no explicit --state.
case " ${FLAGS[*]} " in *" --state "*) : ;; *) FLAGS+=(--state open) ;; esac

gh issue list --repo "$REPO" "${FLAGS[@]}" \
    --json number,title,state,url,body,assignees --limit 200 \
| jq --arg repo "$REPO" '[ .[] | {
    external_id: ($repo + "#" + (.number | tostring)),
    title: .title,
    status: (if (.state | ascii_upcase) == "CLOSED" then "done"
             elif ((.assignees // []) | length) > 0 then "in_progress"
             else "todo" end),
    url: .url,
    description: (.body // "")
  } ]'
