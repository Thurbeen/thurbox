#!/usr/bin/env bats
# Tests for classify.sh — the pure request → action-flag classifier. These feed
# crafted normalized-request JSON on stdin (the provider.sh `list` shape), so
# they run anywhere bats + jq are installed, with no live forge:
#   bats extensions/ci-shepherd/scripts/classify.bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/classify.sh"
}

# Build a one-element request array, overriding base fields via key=value args.
req() {
  jq -nc --args '
    [ { number: 1, title: "t", branch: "feat/x", draft: false,
        review: "none", ci: "OK", rebase: "none" }
      + ( $ARGS.positional | map(split("=") | {(.[0]): (.[1]
            | if . == "true" then true elif . == "false" then false else . end)})
          | add // {} ) ]' "$@"
}

# Classify a crafted request (key=value overrides) — the function `run` invokes.
classify() { req "$@" | "$SCRIPT"; }

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "behind target branch surfaces REBASE" {
  run classify rebase=NEEDED
  [ "$status" -eq 0 ]
  [[ "$output" == *"[REBASE]"* ]]
  [[ "$output" == *"rebase=NEEDED"* ]]
}

@test "merge conflict (diverged) surfaces REBASE" {
  run classify rebase=CONFLICT
  [ "$status" -eq 0 ]
  [[ "$output" == *"[REBASE]"* ]]
  [[ "$output" == *"rebase=CONFLICT"* ]]
}

@test "a clean, up-to-date request is ok" {
  run classify
  [ "$status" -eq 0 ]
  [[ "$output" == *"[ok]"* ]]
}

@test "CI-FAIL outranks REBASE" {
  run classify ci=FAIL rebase=NEEDED
  [ "$status" -eq 0 ]
  [[ "$output" == *"[CI-FAIL]"* ]]
  [[ "$output" != *"[REBASE]"* ]]
}

@test "changes-requested outranks rebase" {
  run classify review=CHANGES_REQUESTED rebase=NEEDED
  [ "$status" -eq 0 ]
  [[ "$output" == *"[CHANGES-REQ]"* ]]
}

@test "draft is never actionable even when behind" {
  run classify draft=true rebase=NEEDED
  [ "$status" -eq 0 ]
  [[ "$output" == *"[draft]"* ]]
}

@test "a missing rebase field defaults to none (ok)" {
  run bash -c "jq -nc '[{number:2,title:\"t\",branch:\"b\",draft:false,review:\"none\",ci:\"OK\"}]' | '$SCRIPT'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"[ok]"* ]]
  [[ "$output" == *"rebase=none"* ]]
}

@test "empty array prints the no-requests placeholder" {
  run bash -c "echo '[]' | '$SCRIPT'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"(no open requests)"* ]]
}
