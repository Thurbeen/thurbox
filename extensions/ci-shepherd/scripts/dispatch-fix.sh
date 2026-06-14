#!/usr/bin/env bash
# dispatch-fix.sh — prepare an isolated worktree on a change request's (PR/MR)
# branch and dispatch a fixer worker against it, in one atomic call.
#
# The ONLY thing this script assumes is **git** (worktree + checkout); every
# forge-specific detail is supplied by the SHEPHERD AGENT, which decides how to
# handle each repo's host. For the built-in fast-path forges (github/gitlab/
# bitbucket) the agent can omit the forge flags and we fill them from
# provider.sh; for any other git forge the agent passes them explicitly.
#
# Usage:
#   dispatch-fix.sh --repo /abs/repo --number 42 [--agent shepherd-worker] \
#     [--title T] [--branch B] [--checkout-cmd CMD] \
#     [--feedback-cmd CMD] [--comment-cmd CMD] \
#     [--rebase] [--base BRANCH] \
#     [--provider auto|github|gitlab|bitbucket|...] [--no-dispatch]
#
# Agent-supplied forge flags (each optional; falls back to the provider fast
# path when the forge is built-in, else REQUIRED for --branch):
#   --branch        the head branch to check out (REQUIRED if forge not built-in)
#   --checkout-cmd  shell run INSIDE the worktree to land the branch
#                   (default: git fetch origin <branch> && git checkout -B …)
#   --feedback-cmd  shell the worker runs to READ review feedback + CI
#   --comment-cmd   shell TEMPLATE the worker runs to post a summary (body appended)
#   --rebase        the request is behind/diverged from its target branch: prepend
#                   a rebase-onto-base step and tell the worker to force-push.
#   --base          the target branch to rebase onto (default: filled from the
#                   provider meta for built-in forges, else the repo's default).
#
# thurbox's own --worktree always passes `git worktree add -b` and so FAILS on a
# branch that already exists — which a request branch always does. So we adopt
# the branch ourselves into a shepherd-owned worktree, then spawn the fixer with
# --repo pointing at it. The fixer pushes straight back to the request branch.

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
WT_ROOT="$HOME_DIR/worktrees"
PROVIDER_SH="$HERE/provider.sh"

REPO="" NUMBER="" AGENT="shepherd-worker" PROVIDER="auto" DISPATCH=1
TITLE="" BRANCH="" CHECKOUT_CMD="" FEEDBACK_CMD="" COMMENT_CMD=""
REBASE=0 BASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)         REPO="$2"; shift 2 ;;
    --number|--pr|--mr) NUMBER="$2"; shift 2 ;;
    --agent)        AGENT="$2"; shift 2 ;;
    --provider)     PROVIDER="$2"; shift 2 ;;
    --title)        TITLE="$2"; shift 2 ;;
    --branch)       BRANCH="$2"; shift 2 ;;
    --checkout-cmd) CHECKOUT_CMD="$2"; shift 2 ;;
    --feedback-cmd) FEEDBACK_CMD="$2"; shift 2 ;;
    --comment-cmd)  COMMENT_CMD="$2"; shift 2 ;;
    --rebase)       REBASE=1; shift ;;
    --base)         BASE="$2"; shift 2 ;;
    --no-dispatch)  DISPATCH=0; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[ -n "$REPO" ] && [ -n "$NUMBER" ] || { echo "--repo and --number are required" >&2; exit 2; }
[ -d "$REPO" ] || { echo "repo not found: $REPO" >&2; exit 2; }
export PROVIDER

prov="$(PROVIDER="$PROVIDER" "$PROVIDER_SH" provider-of "$REPO")"
builtin=0
case "$prov" in github|gitlab|bitbucket) builtin=1 ;; esac

# Fill any forge details the agent didn't supply, using the fast path when the
# forge is built-in. Anything still missing for a non-built-in forge is an error.
if [ "$builtin" -eq 1 ]; then
  if [ -z "$TITLE" ] || [ -z "$BRANCH" ] || { [ "$REBASE" -eq 1 ] && [ -z "$BASE" ]; }; then
    META="$(PROVIDER="$PROVIDER" "$PROVIDER_SH" meta "$REPO" "$NUMBER" 2>&1)" \
      || { echo "provider meta failed: $META" >&2; exit 2; }
    [ -n "$TITLE" ]  || TITLE="$(printf '%s' "$META" | jq -r '.title')"
    [ -n "$BRANCH" ] || BRANCH="$(printf '%s' "$META" | jq -r '.branch')"
    [ -n "$BASE" ]   || BASE="$(printf '%s' "$META" | jq -r '.base // empty')"
  fi
  [ -n "$FEEDBACK_CMD" ] || FEEDBACK_CMD="$(PROVIDER="$PROVIDER" "$PROVIDER_SH" feedback-cmd "$REPO" "$NUMBER")"
  [ -n "$COMMENT_CMD" ]  || COMMENT_CMD="$(PROVIDER="$PROVIDER" "$PROVIDER_SH" comment-cmd "$REPO" "$NUMBER")"
fi
[ -n "$BRANCH" ] || { echo "forge '$prov' is not built-in: pass --branch (and --checkout-cmd/--feedback-cmd/--comment-cmd)" >&2; exit 2; }
[ -n "$TITLE" ]  || TITLE="request #$NUMBER"
# For a rebase we need a target branch; fall back to the repo's default branch.
if [ "$REBASE" -eq 1 ] && [ -z "$BASE" ]; then
  BASE="$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's#^origin/##')"
  [ -n "$BASE" ] || BASE="main"
fi

# Prepare the worktree (git-universal), then land the branch.
mkdir -p "$WT_ROOT"
SLUG="$(basename "$REPO" | tr -c 'A-Za-z0-9_.-' '-')"
WT="$WT_ROOT/${SLUG}-${prov}-${NUMBER}"
(cd "$REPO" && git fetch origin --quiet) || true
if [ ! -d "$WT" ]; then
  git -C "$REPO" worktree add --detach "$WT" >/dev/null 2>&1 \
    || { echo "git worktree add failed for $WT" >&2; exit 2; }
fi
if [ -n "$CHECKOUT_CMD" ]; then
  ( cd "$WT" && eval "$CHECKOUT_CMD" ) || { echo "checkout-cmd failed in $WT" >&2; exit 2; }
elif [ "$builtin" -eq 1 ]; then
  PROVIDER="$PROVIDER" "$PROVIDER_SH" checkout "$REPO" "$WT" "$NUMBER" \
    || { echo "provider checkout of #$NUMBER failed in $WT" >&2; exit 2; }
else
  ( cd "$WT" && git fetch origin "$BRANCH" --quiet && git checkout -B "$BRANCH" FETCH_HEAD ) \
    || { echo "default git checkout of $BRANCH failed in $WT" >&2; exit 2; }
fi

# Worker prompt: use the agent-supplied (or fast-path) commands when present;
# otherwise tell the worker to work them out from the repo's forge itself.
if [ -n "$FEEDBACK_CMD" ]; then
  READ_STEP="$FEEDBACK_CMD"
else
  READ_STEP="Work out this forge from 'git remote -v', then read the request's review feedback and CI status with whatever tooling it provides (its CLI such as gh/glab/tea, or its REST API via curl)."
fi
if [ -n "$COMMENT_CMD" ]; then
  COMMENT_STEP="$COMMENT_CMD \"what I changed…\""
else
  COMMENT_STEP="Post a summary comment on the request using this forge's CLI/API."
fi

# When the request is behind/diverged from its base, prepend a rebase step and
# force-push (history is rewritten) instead of a plain push.
if [ "$REBASE" -eq 1 ]; then
  REBASE_STEP="$(cat <<EOF
0. This request is **behind its target branch \`$BASE\`** and must be brought up
   to date before it can merge (branch protection blocks an out-of-date branch).
   Rebase it onto the latest base first:
     git fetch origin $BASE
     git rebase origin/$BASE
   If the rebase stops on a conflict, resolve each conflict minimally and
   correctly — preserve THIS request's intent, take base for unrelated churn —
   then \`git rebase --continue\`. If the conflicts are genuinely ambiguous, abort
   (\`git rebase --abort\`) and stop with a \`question\` rather than guessing.
EOF
)"
  PUSH_STEP="Commit any fixes, then push the rebased branch with \`git push --force-with-lease\` (the rebase rewrote history) so the request updates."
  ACCEPT_LINE="accept: the branch is rebased onto \`$BASE\` (no longer behind), CI is green, and every changes-requested review thread is addressed."
else
  REBASE_STEP=""
  PUSH_STEP="Commit and push to the SAME branch (\`git push\`) so the request updates."
  ACCEPT_LINE="accept: CI is green and every changes-requested review thread is addressed."
fi

DESC="$(cat <<EOF
You are the fixer for **#$NUMBER** ("$TITLE") on the $prov branch \`$BRANCH\`.
Your working directory is already a worktree checked out on that branch.

$ACCEPT_LINE

Do, in order:
${REBASE_STEP:+$REBASE_STEP
}1. Read the review feedback and CI state:
   $READ_STEP
2. Address the failing checks and/or the changes-requested comments. Make the
   minimal correct change.
3. $PUSH_STEP
4. Post a summary so reviewers know it's handled (do NOT merge):
   $COMMENT_STEP
If you are blocked (a decision only the user can make, missing credentials, an
ambiguous review request), do NOT guess — stop and report it in the result
\`question\` field.

When finished: mark this task done (thurbox-cli task edit <id> --status done),
print a final line \`===RESULT===\` followed by one line of JSON:
{"status":"ok|error","url":"...","notes":"...","question":"..."}
then notify the shepherd so the next request dispatches immediately:
thurbox-cli session send "\$(thurbox-cli session list --json | jq -r '.[] | select(.name=="shepherd") | .id')" "tick"
EOF
)"

CREATED="$(thurbox-cli task create --title "fix #$NUMBER: $TITLE" \
  --description "$DESC" --repo "$WT" --agent "$AGENT")"
printf '%s\n' "$CREATED"

if [ "$DISPATCH" -eq 1 ]; then
  ID="$(printf '%s' "$CREATED" | jq -r .id)"
  thurbox-cli task run "$ID"
fi
