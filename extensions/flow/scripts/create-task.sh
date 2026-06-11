#!/usr/bin/env bash
# create-task.sh — create a thurbox task and (when dispatchable) spawn its
# worker in the SAME call, so capture → session is atomic and immediate.
#
# Usage:
#   create-task.sh --title T --description D            # plain todo
#   create-task.sh --title T --description D \
#     --repo /abs/path --agent flow-worker \
#     [--worktree BRANCH] [--base origin/main] [--no-dispatch]
#
# Worktrees are always based on the REMOTE default branch: with --worktree
# and no --base, origin/main is assumed. When the base is a remote-tracking
# ref (origin/...), the repo is fetched first so the base is current.
#
# Prints the created task JSON; when dispatched, a second JSON line with the
# `task run` outcome ({"spawned": "..."} / {"reused": "..."}).

set -euo pipefail

TITLE="" DESC="" REPO="" AGENT="" WORKTREE="" BASE="" DISPATCH=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --title)       TITLE="$2"; shift 2 ;;
    --description) DESC="$2"; shift 2 ;;
    --repo)        REPO="$2"; shift 2 ;;
    --agent)       AGENT="$2"; shift 2 ;;
    --worktree)    WORKTREE="$2"; shift 2 ;;
    --base)        BASE="$2"; shift 2 ;;
    --no-dispatch) DISPATCH=0; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$TITLE" ]] || { echo "--title is required" >&2; exit 2; }

ARGS=(task create --title "$TITLE")
[[ -n "$DESC" ]] && ARGS+=(--description "$DESC")
if [[ -n "$REPO" ]]; then
  [[ -n "$AGENT" ]] || { echo "--repo requires --agent" >&2; exit 2; }
  ARGS+=(--repo "$REPO" --agent "$AGENT")
  if [[ -n "$WORKTREE" ]]; then
    [[ -n "$BASE" ]] || BASE="origin/main"
    ARGS+=(--worktree "$WORKTREE" --base "$BASE")
    # A remote-tracking base is only as fresh as the last fetch.
    if [[ "$BASE" == origin/* ]]; then
      git -C "$REPO" fetch origin --quiet 2>/dev/null || true
    fi
  fi
fi

CREATED="$(thurbox-cli "${ARGS[@]}")"
printf '%s\n' "$CREATED"

if [[ -n "$REPO" && "$DISPATCH" -eq 1 ]]; then
  ID="$(printf '%s' "$CREATED" | jq -r .id)"
  thurbox-cli task run "$ID"
fi
