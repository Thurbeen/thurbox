#!/usr/bin/env bash
# classify.sh — pure request → action-flag classifier for ci-shepherd.
#
# Reads a normalized request JSON array (the provider.sh `list` shape) on stdin
# and prints one formatted line per request with its derived action flag. Kept
# separate from shepherd-snapshot.sh so the flag precedence is unit-testable
# (`bats classify.bats`) without a live forge. The input array is one repo's
# open requests, so the rebase ordering below is naturally scoped per repo.
#
# Flag precedence (highest first):
#   draft          — work in progress, never actionable
#   CHANGES-REQ    — a reviewer asked for changes
#   CI-FAIL        — checks are red
#   REBASE         — behind the target branch / diverged (rebase.NEEDED|CONFLICT);
#                    branch protection blocks the merge until it's brought up to date
#   REBASE-QUEUED  — same as REBASE, but another REBASE request in this repo goes
#                    first (see "Rebase serialization" below) — hold, don't dispatch
#   ok             — nothing to do
#
# CI-FAIL outranks REBASE on purpose: fix the red checks first; a rebase re-runs
# CI anyway, so it's the last gate before a clean PR can merge.
#
# Rebase serialization (per repo): when "require branches up to date" protection
# is on, REBASE-only requests in the same repo mutually invalidate each other —
# rebasing them all at once means whichever merges first knocks the rest back out
# of date, so you pay O(n^2) rebases + CI runs. To merge them fastest we rebase
# ONE at a time: the lowest-numbered REBASE request (oldest, most likely already
# reviewed = closest to merge) keeps the actionable `REBASE` flag; every other
# REBASE request is demoted to `REBASE-QUEUED` and held until the active one
# merges and the next tick advances the queue. CI-FAIL / CHANGES-REQ requests are
# never queued — they need independent work and dispatch in parallel; they join
# the rebase queue only once they become REBASE-only.

set -euo pipefail

jq -r '
  if length == 0 then "  (no open requests)" else
  # Attach the derived flag to every request first, so the rebase queue can be
  # computed across the whole repo before any line is printed.
  [ .[]
    | . + { _rb: (.rebase // "none") }
    | . + { _flag:
        (if .draft then "draft"
         elif .review == "CHANGES_REQUESTED" then "CHANGES-REQ"
         elif .ci == "FAIL" then "CI-FAIL"
         elif ._rb == "NEEDED" or ._rb == "CONFLICT" then "REBASE"
         else "ok" end) }
  ] as $reqs
  # The active rebase target: the lowest-numbered REBASE request merges first.
  | ( [ $reqs[] | select(._flag == "REBASE") | .number ] | sort | first ) as $rb_next
  | $reqs[]
  | ( ._flag == "REBASE" and .number != $rb_next ) as $queued
  | "  #\(.number)  [\(if $queued then "REBASE-QUEUED" else ._flag end)]  \(.title)  (head=\(.branch), CI=\(.ci), review=\(.review), rebase=\(._rb))"
    + ( if $queued then "  (queued behind #\($rb_next))" else "" end )
  end'
