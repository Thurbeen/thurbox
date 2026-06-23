#!/usr/bin/env bats
# Tests for link-sessions.sh — the pure (requests × sessions) head-branch join.
# They feed crafted normalized-request JSON on stdin and a crafted session-list
# JSON file as $1, so they run anywhere bats + jq are installed, with no live
# forge or tmux:
#   bats extensions/ci-shepherd/scripts/link-sessions.bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/link-sessions.sh"
  SESS="${BATS_TEST_TMPDIR}/sessions.json"
}

# A one-element request array on branch $1 (default feat/x).
req() {
  jq -nc --arg b "${1:-feat/x}" '[ { number: 7, title: "t", branch: $b } ]'
}

# Write a session-list JSON file: each arg is "name:branch". The branch is the
# text after the LAST colon, so a session name may itself contain colons
# (e.g. "fix #7: red CI · #42:feat/x" → name "fix #7: red CI · #42", branch
# "feat/x"). A worktree with an empty branch is written as branch "".
sessions() {
  local out='[]' name branch
  for spec in "$@"; do
    name="${spec%:*}"; branch="${spec##*:}"
    out="$(printf '%s' "$out" | jq -c --arg n "$name" --arg b "$branch" \
      '. + [{ name: $n, id: "id-\($n)", cwd: "/repo", worktrees: [{ branch: $b }] }]')"
  done
  printf '%s' "$out" > "$SESS"
}

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "a user session on the head branch is flagged" {
  sessions "mywork:feat/x"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"#7"* ]]
  [[ "$output" == *"head=feat/x"* ]]
  [[ "$output" == *"mywork"* ]]
}

@test "no session on the branch prints nothing" {
  sessions "other:feat/unrelated"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "the shepherd's own fixer session is excluded (task- prefix)" {
  sessions "task-42-fix:feat/x"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "a current-style worker session (· #id) is excluded" {
  sessions "fix #7: red CI · #42:feat/x"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "the shepherd monitor session is excluded" {
  sessions "shepherd:feat/x"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "a session merely containing 'task' (not the task- prefix) still matches" {
  sessions "refactor-tasks:feat/x"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"refactor-tasks"* ]]
}

@test "a multi-repo session matches on a non-first worktree branch" {
  printf '%s' '[{"name":"multi","id":"id-m","cwd":"/repo",
    "worktrees":[{"branch":"other"},{"branch":"feat/x"}]}]' > "$SESS"
  run bash -c "'$SCRIPT' '$SESS' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"multi"* ]]
}

@test "no sessions file (default /dev/null) is safe" {
  run bash -c "'$SCRIPT' <<<'$(req feat/x)'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "a request with no branch is skipped, not matched" {
  sessions "mywork:" # empty branch on the session too
  run bash -c "echo '[{\"number\":9,\"title\":\"t\",\"branch\":\"\"}]' | '$SCRIPT' '$SESS'"
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}
