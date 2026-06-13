#!/usr/bin/env bats
# Tests for parse-questions.sh — extracting a worker's pending ===QUESTIONS===
# block. Pure stdin parsing, no thurbox-cli, so they run anywhere bats + jq are
# installed: `bats extensions/flow/scripts/parse-questions.bats`.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/parse-questions.sh"
}

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "pending questions block is found and printed" {
  run bash -c 'printf "planning...\n===QUESTIONS===\n{\"questions\":[\"a\",\"b\",\"c\"]}\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [[ "$output" == *'"questions"'* ]]
  [[ "$output" == *'"a"'* ]]
}

@test "no questions block → exit 1" {
  run bash -c 'printf "just some output\nno sentinel here\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 1 ]
}

@test "questions followed by a RESULT are no longer pending → exit 1" {
  run bash -c 'printf "===QUESTIONS===\n{\"questions\":[\"a\"]}\nmore work\n===RESULT===\n{\"status\":\"ok\"}\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 1 ]
}

@test "malformed JSON after the sentinel → exit 2" {
  run bash -c 'printf "===QUESTIONS===\n{not json}\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 2 ]
}

@test "empty questions array → exit 2" {
  run bash -c 'printf "===QUESTIONS===\n{\"questions\":[]}\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 2 ]
}

@test "only the LAST questions block is reported" {
  run bash -c 'printf "===QUESTIONS===\n{\"questions\":[\"old\"]}\n===QUESTIONS===\n{\"questions\":[\"new\"]}\n" | "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [[ "$output" == *'"new"'* ]]
  [[ "$output" != *'"old"'* ]]
}
