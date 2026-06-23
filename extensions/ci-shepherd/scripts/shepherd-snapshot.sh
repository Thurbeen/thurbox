#!/usr/bin/env bash
# shepherd-snapshot.sh — one-call view of the ci-shepherd's world: every open
# change request (PR/MR) across the watched repos (with a derived action flag),
# plus the live fixer tasks/sessions. Host-neutral: all forge specifics go
# through ./provider.sh, so GitHub/GitLab/Bitbucket all funnel into one shape.
#
# Reads the watch list from ../repos.md: a markdown table with columns
#   | name | path | author | provider |
# `author` defaults to @me (your own requests). `provider` defaults to auto
# (detected from the git remote).

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
REPOS_MD="$HOME_DIR/repos.md"
WT_DIR="$HOME_DIR/worktrees"

trim() { printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# Snapshot the live session list ONCE so each request can be cross-referenced
# against any thurbox session already working on its head branch (link-sessions.sh).
SESS_FILE="$(mktemp -t shepherd-sessions.XXXXXX)"
trap 'rm -f "$SESS_FILE"' EXIT
thurbox-cli session list --json 2>/dev/null > "$SESS_FILE" || echo '[]' > "$SESS_FILE"

echo "## change requests (watched repos)"
if [ ! -f "$REPOS_MD" ]; then
  echo "  (no repos.md — add your repos to $REPOS_MD)"
else
  # Parse table rows, skipping the header and the |---| separator.
  grep '^[[:space:]]*|' "$REPOS_MD" \
    | grep -viE '^\s*\|\s*name\s*\|' \
    | grep -vE '^\s*\|[[:space:]]*-' \
    | while IFS='|' read -r _ name path author provider _; do
        name="$(trim "$name")"; path="$(trim "$path")"
        author="$(trim "$author")"; provider="$(trim "$provider")"
        [ -n "$path" ] || continue
        [ -n "$author" ] || author="@me"
        [ -n "$provider" ] || provider="auto"
        if [ ! -d "$path" ]; then
          echo "### ${name:-$path}  ($path)"; echo "  (path not found)"; continue
        fi
        prov="$(PROVIDER="$provider" "$HERE/provider.sh" provider-of "$path" 2>/dev/null || echo unknown)"
        echo "### ${name:-$path}  ($path)  [$prov]"
        case "$prov" in
          github|gitlab|bitbucket) : ;;  # built-in fast path below
          *)
            # No built-in adapter: hand the agent the raw materials so it can
            # list this repo's requests itself during the tick (author=$author).
            PROVIDER="$provider" "$HERE/provider.sh" describe "$path" 2>/dev/null | sed 's/^/  /'
            echo "  ACTION: no built-in adapter — list this repo's open requests yourself (author=$author)"
            continue ;;
        esac
        CRS="$(PROVIDER="$provider" "$HERE/provider.sh" list "$path" "$author" 2>&1)" \
          || { echo "  (provider error: $(printf '%s' "$CRS" | head -1))"; continue; }
        printf '%s' "$CRS" | "$HERE/classify.sh" 2>/dev/null \
          || echo "  (could not parse provider output)"
        # Flag any request whose head branch already has a live, non-fixer
        # thurbox session — hands-on work the shepherd must not race.
        printf '%s' "$CRS" | "$HERE/link-sessions.sh" "$SESS_FILE" 2>/dev/null || true
      done
fi

echo
echo "## fixer tasks"
if TASKS="$(thurbox-cli task list 2>/dev/null)"; then
  printf '%s' "$TASKS" | jq -r '
    [.[] | select(.title | startswith("fix #"))] |
    if length == 0 then "  (none)" else
      .[] | "  #\(.id) [\(.status)] \(.title)  {repo=\(.action.repo_path // "?")}"
    end' 2>/dev/null || true
else
  echo "  (thurbox-cli task list failed)"
fi

echo
echo "## sessions (shepherd / workers)"
# Worker sessions are named "<title> · #<id>" (current) or "task-<id>[-…]"
# (legacy) — mirror flow-snapshot.sh / Task::matches_spawn_session.
jq -r '
  .[] | select(.name == "shepherd"
               or (.name | startswith("task-"))
               or (.name | test(" · #[0-9]+$"))) |
  "  \(.name)  \(.id)  agent=\(.agent)  cwd=\(.cwd)"' "$SESS_FILE" 2>/dev/null || true

echo
echo "## fixer worktrees ($WT_DIR)"
if [ -d "$WT_DIR" ]; then
  find "$WT_DIR" -maxdepth 1 -mindepth 1 -type d -exec basename {} \; | sed 's/^/  /' || true
else
  echo "  (none)"
fi
