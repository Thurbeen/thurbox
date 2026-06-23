#!/usr/bin/env bash
# link-sessions.sh — flag change requests that already have a *live, non-fixer*
# thurbox session on their head branch.
#
# The shepherd dispatches a fixer per actionable request, but a PR/MR may
# already be getting hands-on work in another thurbox session — one the user
# (or another agent) opened on that branch. That session is a *worker*, not a
# blocker: the shepherd shouldn't dispatch a duplicate fixer (two worktrees
# force-pushing one branch would clobber each other), but it should keep
# *monitoring* the request and fold it into the per-repo merge ordering (the
# live session holds that repo's rebase slot). This join surfaces the overlap
# so the shepherd can do that instead of dispatching over it.
#
# Reads the normalized request JSON array (the provider.sh `list` shape) on
# stdin and takes the `thurbox-cli session list --json` array as $1 (a file
# path; default /dev/null = no sessions). Prints one line per (request,
# session) overlap, nothing when there is none:
#   "  ⮑ #<n> head=<branch> already has a live session: <name>  <id>  cwd=<cwd>"
#
# Kept separate from shepherd-snapshot.sh so the match logic is unit-testable
# (`bats link-sessions.bats`) without a live forge or running tmux.
#
# Fixer/shepherd sessions are *excluded* (shepherd, task-*, "… · #<id>"): those
# ARE the shepherd's own dispatch result, already tracked via the `fix #<n>`
# tasks — counting them would flag every dispatched PR. The point is to spot
# OTHER work on the branch.

set -euo pipefail

SESS_FILE="${1:-/dev/null}"

jq -r --slurpfile sess "$SESS_FILE" '
  ($sess[0] // []) as $sessions
  | .[]
  | . as $req
  | ($req.branch // "") as $head
  | select($head != "")
  | $sessions[]
  | select((.name == "shepherd"
            or (.name | startswith("task-"))
            or (.name | test(" · #[0-9]+$"))) | not)
  | select(any((.worktrees // [])[]; .branch == $head))
  | "  ⮑ #\($req.number) head=\($head) already has a live session: " +
    "\(.name)  \(.id)  cwd=\(.cwd // "?")"
'
