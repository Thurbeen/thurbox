#!/usr/bin/env bash
# rebase-check.sh — authoritative, git-local "is this head behind its base?" check.
#
# The forges compute their merge state lazily and asynchronously (GitHub's
# mergeStateStatus, GitLab's diverged_commits_count), so an API read frequently
# returns a stale/UNKNOWN value and a genuinely behind branch never gets flagged.
# This script answers the same question from git directly: given two already-
# resolvable refs (typically remote-tracking refs the caller just fetched), it
# tells whether <head> needs to be brought up to date with <base>.
#
# Usage:  rebase-check.sh <repo> <base-ref> <head-ref>
#
# Prints exactly one of:
#   none      — <base> is already contained in <head>; up to date, nothing to do
#   NEEDED    — <head> is behind <base> and a rebase would apply cleanly
#   CONFLICT  — <head> has diverged from <base> so far it no longer merges cleanly
#   unknown   — a ref could not be resolved (e.g. a fork head not on origin);
#               the caller should fall back to the forge's own value (exit 3)
#
# It is pure and side-effect-free over the two refs (no fetch, no working-tree
# mutation), so it unit-tests against a throwaway repo with no live forge.

set -euo pipefail

REPO="${1:-}"; BASE="${2:-}"; HEAD="${3:-}"
[ -n "$REPO" ] && [ -n "$BASE" ] && [ -n "$HEAD" ] \
  || { echo "usage: rebase-check.sh <repo> <base-ref> <head-ref>" >&2; exit 2; }
[ -d "$REPO" ] || { echo "repo not found: $REPO" >&2; exit 2; }

git_q() { git -C "$REPO" "$@"; }

# Both refs must resolve locally; an unresolvable head (cross-repo/fork PR whose
# branch was never pushed to origin) is reported as `unknown` so the caller keeps
# whatever the forge said rather than mis-classifying it.
git_q rev-parse --verify --quiet "${BASE}^{commit}" >/dev/null 2>&1 \
  || { echo unknown; exit 3; }
git_q rev-parse --verify --quiet "${HEAD}^{commit}" >/dev/null 2>&1 \
  || { echo unknown; exit 3; }

# Up to date: base is already an ancestor of head, so head has every base commit.
if git_q merge-base --is-ancestor "$BASE" "$HEAD"; then
  echo none
  exit 0
fi

# Behind base. Decide NEEDED vs CONFLICT by test-merging base into head without
# touching the working tree. `merge-tree --write-tree` (git >= 2.38) exits 0 on a
# clean merge, 1 on conflicts, >1 on usage error (older git). A merge conflict is
# a close proxy for a rebase conflict; on older git we can still report NEEDED
# since we already know the branch is behind.
set +e
git_q merge-tree --write-tree "$HEAD" "$BASE" >/dev/null 2>&1
rc=$?
set -e
case "$rc" in
  0) echo NEEDED ;;
  1) echo CONFLICT ;;
  *) echo NEEDED ;;
esac
