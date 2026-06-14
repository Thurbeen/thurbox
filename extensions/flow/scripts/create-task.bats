#!/usr/bin/env bats
# Tests for create-task.sh's plan-first prompt composition. These exercise the
# pure `--dry-run` path (no thurbox-cli, no side effects), so they run anywhere
# bats is installed: `bats extensions/flow/scripts/create-task.bats`.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/create-task.sh"
}

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "worker dispatch composes header, planning phase, and footer" {
  run "$SCRIPT" --title "Add rate limiting" \
    --description "Throttle the API." \
    --repo /tmp/repo --agent flow-worker \
    --accept "requests over the limit get 429" --priority high \
    --worktree flow/add-rate-limiting --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"priority: high"* ]]
  [[ "$output" == *"repo: /tmp/repo"* ]]
  [[ "$output" == *"accept: requests over the limit get 429"* ]]
  [[ "$output" == *"Throttle the API."* ]]
  [[ "$output" == *"## Planning phase"* ]]
  [[ "$output" == *"at least 3"* ]]
  [[ "$output" == *"message send --to flow --kind questions"* ]]
  [[ "$output" == *"message send --to flow --kind plan"* ]]
  [[ "$output" == *"## Problem"* ]]
  [[ "$output" == *"## Acceptance criteria"* ]]
  [[ "$output" == *"## Approach"* ]]
  # The interactive plan-mode modal is gone (it stalls headless workers).
  [[ "$output" != *"EnterPlanMode"* ]]
  [[ "$output" != *"===QUESTIONS==="* ]]
  [[ "$output" == *"branch flow/add-rate-limiting"* ]]
  [[ "$output" == *"message send --to flow --kind result"* ]]
}

@test "--no-plan drops only the planning phase, keeps header and footer" {
  run "$SCRIPT" --title "Fix typo" --description "readme typo" \
    --repo /tmp/repo --agent flow-worker --accept "typo fixed" \
    --worktree flow/fix-typo --no-plan --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"accept: typo fixed"* ]]
  [[ "$output" != *"## Planning phase"* ]]
  [[ "$output" != *"--kind questions"* ]]
  # Footer (the result report) stays even with the planning phase dropped.
  [[ "$output" == *"message send --to flow --kind result"* ]]
}

@test "--accept without a repo composes the header only (no planning, no footer)" {
  run "$SCRIPT" --title "Idea" --description "some notes" \
    --accept "decided one way or the other" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"priority: normal"* ]]   # default priority
  [[ "$output" == *"repo: unknown"* ]]       # default repo
  [[ "$output" == *"accept: decided one way or the other"* ]]
  [[ "$output" == *"some notes"* ]]
  [[ "$output" != *"## Planning phase"* ]]   # gated on a worker dispatch
  [[ "$output" != *"--kind result"* ]]       # footer is worker-only
}

@test "plain todo without --accept keeps the description verbatim" {
  run "$SCRIPT" --title "Revisit caching" \
    --description "low priority idea" --dry-run
  [ "$status" -eq 0 ]
  [ "$output" = "low priority idea" ]
}

@test "--title is required" {
  run "$SCRIPT" --description "no title" --dry-run
  [ "$status" -eq 2 ]
}
