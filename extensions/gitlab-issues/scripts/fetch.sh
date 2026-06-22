#!/usr/bin/env bash
# fetch.sh — list a GitLab project's issues as a normalized JSON array for upsert.sh.
#
# Takes ONE argument: the tracker row's `query`, whose first word is the project
# (`group/project`) and whose remaining words are extra `glab issue list` flags:
#   fetch.sh "group/project --assignee=@me --label=bug"   # open by default
#   fetch.sh "group/project --all"                         # include closed
#
# Output element (the contract upsert.sh consumes):
#   { "external_id": "group/project#<iid>",   # unique under source=gitlab
#     "title": "..", "status": "todo|in_progress|done",
#     "url": "..", "description": ".." }
# Status mapping: closed → done; opened with an assignee → in_progress; opened → todo.

set -euo pipefail

command -v glab >/dev/null 2>&1 || { echo "glab (GitLab CLI) not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

QUERY="${1:?usage: fetch.sh \"<group/project> [glab issue list flags]\"}"
# Word-split the query: project + passthrough flags.
read -ra PARTS <<<"$QUERY"
PROJECT="${PARTS[0]:?query must start with group/project}"
FLAGS=("${PARTS[@]:1}")
# `glab issue list` defaults to open issues; pass --all (-A) or --closed (-c) in
# the query to widen it. (No --state flag exists, unlike `gh issue list`.)

glab issue list -R "$PROJECT" -O json "${FLAGS[@]}" \
| jq --arg project "$PROJECT" '[ .[] | {
    external_id: ($project + "#" + (.iid | tostring)),
    title: .title,
    status: (if .state == "closed" then "done"
             elif ((.assignees // []) | length) > 0 then "in_progress"
             else "todo" end),
    url: .web_url,
    description: (.description // "")
  } ]'
