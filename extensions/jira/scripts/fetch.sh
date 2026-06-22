#!/usr/bin/env bash
# fetch.sh — list a Jira project's issues as a normalized JSON array for upsert.sh.
#
# Takes ONE argument: the tracker row's `query`, which is a whole JQL string
# (it contains spaces — it is NOT word-split, it is passed as one JQL value):
#   fetch.sh "project = ENG AND statusCategory != Done"
#
# Output element (the contract upsert.sh consumes):
#   { "external_id": "ENG-7",                 # the issue key, unique under source=jira
#     "title": "..", "status": "todo|in_progress|done",
#     "url": "..", "description": "" }
# Status mapping (Jira statusCategory key): done → done; indeterminate →
# in_progress; new (or anything else) → todo. Descriptions are Jira v3 ADF
# (rich JSON); converting to text is out of scope, so description is left blank.
#
# Auth: JIRA_BASE_URL (e.g. https://your.atlassian.net), JIRA_EMAIL,
# JIRA_API_TOKEN. Uses HTTP Basic auth over the Jira Cloud REST API v3.

set -euo pipefail

command -v curl >/dev/null 2>&1 || { echo "curl not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

: "${JIRA_BASE_URL:?set JIRA_BASE_URL (e.g. https://your.atlassian.net)}"
: "${JIRA_EMAIL:?set JIRA_EMAIL (your Atlassian account email)}"
: "${JIRA_API_TOKEN:?set JIRA_API_TOKEN (an Atlassian API token)}"

JQL="${1:?usage: fetch.sh \"<JQL>\"}"

jira_get() { curl -fsSL -u "$JIRA_EMAIL:$JIRA_API_TOKEN" -H 'Accept: application/json' "$@"; }

# Jira Cloud removed the old /rest/api/3/search (HTTP 410); the replacement is
# /rest/api/3/search/jql (same response shape: {issues:[{key,fields:{…}}]}).
jira_get -G "$JIRA_BASE_URL/rest/api/3/search/jql" \
    --data-urlencode "jql=$JQL" \
    --data-urlencode "maxResults=200" \
    --data-urlencode "fields=summary,status" \
| jq --arg base "$JIRA_BASE_URL" '[ .issues[] | {
    external_id: .key,
    title: .fields.summary,
    status: (.fields.status.statusCategory.key
             | if . == "done" then "done"
               elif . == "indeterminate" then "in_progress"
               else "todo" end),
    url: ($base + "/browse/" + .key),
    description: ""
  } ]'
