#!/usr/bin/env bash
# classify.sh — pure request → action-flag classifier for ci-shepherd.
#
# Reads a normalized request JSON array (the provider.sh `list` shape) on stdin
# and prints one formatted line per request with its derived action flag. Kept
# separate from shepherd-snapshot.sh so the flag precedence is unit-testable
# (`bats classify.bats`) without a live forge.
#
# Flag precedence (highest first):
#   draft        — work in progress, never actionable
#   CHANGES-REQ  — a reviewer asked for changes
#   CI-FAIL      — checks are red
#   REBASE       — behind the target branch / diverged (rebase.NEEDED|CONFLICT);
#                  branch protection blocks the merge until it's brought up to date
#   ok           — nothing to do
#
# CI-FAIL outranks REBASE on purpose: fix the red checks first; a rebase re-runs
# CI anyway, so it's the last gate before a clean PR can merge.

set -euo pipefail

jq -r '
  if length == 0 then "  (no open requests)" else
  .[] |
  ((.rebase // "none")) as $rb |
  (if .draft then "draft"
   elif .review == "CHANGES_REQUESTED" then "CHANGES-REQ"
   elif .ci == "FAIL" then "CI-FAIL"
   elif $rb == "NEEDED" or $rb == "CONFLICT" then "REBASE"
   else "ok" end) as $flag |
  "  #\(.number)  [\($flag)]  \(.title)  (head=\(.branch), CI=\(.ci), review=\(.review), rebase=\($rb))"
  end'
