#!/usr/bin/env bash
# fetch.sh — list a Linear team's issues as a normalized JSON array for upsert.sh.
#
# Takes ONE argument: the tracker row's `query`, which is a Linear **team key**
# (e.g. `ENG`). The whole arg is treated as the team key (trimmed):
#   fetch.sh "ENG"
#
# Output element (the contract upsert.sh consumes):
#   { "external_id": "ENG-7",                  # the issue identifier, globally
#     "title": "..", "status": "todo|in_progress|done",   # unique under source=linear
#     "url": "..", "description": ".." }
# Status mapping (Linear state types triage/backlog/unstarted/started/completed/
# canceled): completed|canceled → done; started → in_progress; else → todo.

set -euo pipefail

command -v curl >/dev/null 2>&1 || { echo "curl not installed" >&2; exit 3; }
command -v jq >/dev/null 2>&1 || { echo "jq not installed" >&2; exit 3; }

: "${LINEAR_API_KEY:?LINEAR_API_KEY is unset (set your Linear personal API key)}"

QUERY="${1:?usage: fetch.sh \"<team-key>\"}"
# The whole query arg is the team key; trim surrounding whitespace.
KEY="$(printf '%s' "$QUERY" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
[ -n "$KEY" ] || { echo "query must be a Linear team key (e.g. ENG)" >&2; exit 2; }

# Call the Linear GraphQL API. The personal API key is sent VERBATIM in the
# Authorization header (Linear keys are NOT prefixed with "Bearer").
linear_gql() { # <graphql-query-json-payload on $1>
  curl -fsSL -X POST https://api.linear.app/graphql \
    -H "Authorization: $LINEAR_API_KEY" -H "Content-Type: application/json" \
    --data "$1"
}

# Build the request body with jq so the team key is JSON-encoded safely. We pass
# the key as a GraphQL variable rather than interpolating it into the query text,
# so a stray quote/brace in the key can never break the query.
# shellcheck disable=SC2016  # $key is a GraphQL variable, not a shell expansion.
gql='query($key: String!) { issues(filter: { team: { key: { eq: $key } } }, first: 200) { nodes { identifier title url description state { type } } } }'
payload="$(jq -n --arg q "$gql" --arg key "$KEY" '{query: $q, variables: {key: $key}}')"

linear_gql "$payload" \
| jq '[ .data.issues.nodes[] | {
    external_id: .identifier,
    title: .title,
    status: (.state.type | if . == "completed" or . == "canceled" then "done"
             elif . == "started" then "in_progress"
             else "todo" end),
    url: .url,
    description: (.description // "")
  } ]'
