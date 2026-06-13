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
  [[ "$output" == *"**Problem**"* ]]
  [[ "$output" == *"**Acceptance criteria**"* ]]
  [[ "$output" == *"**Approach**"* ]]
  [[ "$output" == *"branch flow/add-rate-limiting"* ]]
  [[ "$output" == *"===RESULT==="* ]]
}

@test "--no-plan drops only the planning phase, keeps header and footer" {
  run "$SCRIPT" --title "Fix typo" --description "readme typo" \
    --repo /tmp/repo --agent flow-worker --accept "typo fixed" \
    --worktree flow/fix-typo --no-plan --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"accept: typo fixed"* ]]
  [[ "$output" != *"## Planning phase"* ]]
  [[ "$output" == *"===RESULT==="* ]]
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
