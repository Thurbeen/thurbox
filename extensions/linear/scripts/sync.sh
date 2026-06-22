#!/usr/bin/env bash
# sync.sh — the deterministic one-shot reconcile run by the github-issues-tick
# automation (an `AutomationAction::Exec` command — no agent, no session, no
# tokens). thurbox's scheduler runs this every 15 min (TUI or headless
# heartbeat) and records its output in the automation run history.
#
# Order is push-then-pull so local status changes reach GitHub before the pull
# reads state back:
#   1. push-status.sh — push thurbox task status to GitHub (push_back=yes rows)
#   2. for each tracker row: fetch.sh "<query>" | upsert.sh --source github
#
# Provider-neutral except for SOURCE; every task-integration extension ships an
# identical copy (only SOURCE differs).

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
TRACKERS_MD="$HOME_DIR/trackers.md"
SOURCE="linear"

# Load provider credentials if present (Linear/Jira need API keys; gh/glab use
# their own stored auth, so a github/gitlab home usually has no such file).
if [ -f "$HOME_DIR/credentials.env" ]; then
  set -a
  # shellcheck disable=SC1091  # user-provided runtime file, not present at lint.
  . "$HOME_DIR/credentials.env"
  set +a
fi

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# 1) Push local status changes back (push-status.sh reads trackers.md itself and
#    only touches push_back=yes rows). Non-fatal: a push error still lets the
#    pull run.
"$HERE/push-status.sh" || echo "sync[$SOURCE]: push-status failed (continuing)"

# 2) Pull every tracker row into the task list.
if [ ! -f "$TRACKERS_MD" ]; then
  echo "sync[$SOURCE]: no trackers.md at $TRACKERS_MD"
  exit 0
fi
while IFS='|' read -r _ _name query _push _; do
  query="$(trim "$query")"
  [ -n "$query" ] || continue
  "$HERE/fetch.sh" "$query" | "$HERE/upsert.sh" --source "$SOURCE"
done < <(grep '^[[:space:]]*|' "$TRACKERS_MD" \
          | grep -viE '^\s*\|\s*name\s*\|' \
          | grep -vE '^\s*\|[[:space:]]*-')

echo "sync[$SOURCE]: done"
