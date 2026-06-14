#!/usr/bin/env bash
# renovate-snapshot.sh — one-call view of the renovate monitor's world: the
# watched repos (with their update strategy and any in-flight renovate branches),
# plus the live update tasks/sessions. Entirely LOCAL — it never queries a forge,
# so a tick stays fast; the worker is the only thing that runs Renovate.
#
# Reads the watch list from ../repos.md: a markdown table with columns
#   | name | path | strategy | provider |
# `strategy` defaults to minor; `provider` defaults to auto.

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
REPOS_MD="$HOME_DIR/repos.md"

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

echo "## watched repos"
if [ ! -f "$REPOS_MD" ]; then
  echo "  (no repos.md — add your repos to $REPOS_MD)"
else
  # Parse table rows, skipping the header and the |---| separator.
  grep '^[[:space:]]*|' "$REPOS_MD" \
    | grep -viE '^\s*\|\s*name\s*\|' \
    | grep -vE '^\s*\|[[:space:]]*-' \
    | while IFS='|' read -r _ name path strategy provider _; do
        name="$(trim "$name")"; path="$(trim "$path")"
        strategy="$(trim "$strategy")"; provider="$(trim "$provider")"
        [ -n "$path" ] || continue
        [ -n "$strategy" ] || strategy="minor"
        [ -n "$provider" ] || provider="auto"
        if [ ! -d "$path" ]; then
          echo "### ${name:-$path}  ($path)"; echo "  (path not found)"; continue
        fi
        echo "### ${name:-$path}  ($path)  [strategy=$strategy, provider=$provider]"
        # In-flight renovate branches in this repo (a worker is updating, or a
        # finished branch awaits review/merge).
        BR="$(git -C "$path" branch --list 'renovate/*' 2>/dev/null | sed 's/^[* ]*//' || true)"
        if [ -n "$BR" ]; then
          printf '%s\n' "$BR" | sed 's/^/  branch: /'
        else
          echo "  branch: (none — eligible for a fresh update run)"
        fi
      done
fi

echo
echo "## update tasks"
if TASKS="$(thurbox-cli task list 2>/dev/null)"; then
  printf '%s' "$TASKS" | jq -r '
    [.[] | select(.title | startswith("update "))] |
    if length == 0 then "  (none)" else
      .[] | "  #\(.id) [\(.status)] \(.title)  {repo=\(.action.repo_path // "?")}"
    end' 2>/dev/null || true
else
  echo "  (thurbox-cli task list failed)"
fi

echo
echo "## sessions (renovate / task-*)"
thurbox-cli session list --json 2>/dev/null | jq -r '
  .[] | select(.name == "renovate" or (.name | startswith("task-"))) |
  "  \(.name)  \(.id)  agent=\(.agent)  cwd=\(.cwd)"' 2>/dev/null || true
