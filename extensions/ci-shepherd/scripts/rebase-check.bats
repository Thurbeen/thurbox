#!/usr/bin/env bats
# Tests for rebase-check.sh — the git-local "is this head behind its base?" check.
# Each test builds a throwaway repo with a known base/head topology, so they run
# anywhere git is installed with no live forge:
#   bats extensions/ci-shepherd/scripts/rebase-check.bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/rebase-check.sh"
  REPO="$BATS_TEST_TMPDIR/repo"
  mkdir -p "$REPO"
  git -C "$REPO" init -q -b main
  git -C "$REPO" config user.email t@t.t
  git -C "$REPO" config user.name t
  # Seed: a base branch `main` with one commit, and a `feat` branch off it.
  printf 'base\n' > "$REPO/file.txt"
  git -C "$REPO" add file.txt
  git -C "$REPO" commit -qm "base commit"
  git -C "$REPO" branch feat
}

# Advance main by one commit (so feat falls behind). $1 = the file content added.
advance_main() {
  printf '%s\n' "$1" >> "$REPO/file.txt"
  git -C "$REPO" add file.txt
  git -C "$REPO" commit -qm "advance main"
}

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "up to date (base is an ancestor of head) is none" {
  run "$SCRIPT" "$REPO" main feat
  [ "$status" -eq 0 ]
  [ "$output" = "none" ]
}

@test "head behind base with a clean rebase is NEEDED" {
  # main moves ahead on a NEW line; feat is untouched -> behind but no conflict.
  printf 'base\nmore\n' > "$REPO/file.txt"
  git -C "$REPO" add file.txt
  git -C "$REPO" commit -qm "advance main cleanly"
  run "$SCRIPT" "$REPO" main feat
  [ "$status" -eq 0 ]
  [ "$output" = "NEEDED" ]
}

@test "head diverged from base into a conflict is CONFLICT" {
  # Both branches edit the SAME line differently -> a merge/rebase conflicts.
  git -C "$REPO" checkout -q feat
  printf 'feat-change\n' > "$REPO/file.txt"
  git -C "$REPO" add file.txt
  git -C "$REPO" commit -qm "feat edits the line"
  git -C "$REPO" checkout -q main
  printf 'main-change\n' > "$REPO/file.txt"
  git -C "$REPO" add file.txt
  git -C "$REPO" commit -qm "main edits the same line"
  run "$SCRIPT" "$REPO" main feat
  [ "$status" -eq 0 ]
  [ "$output" = "CONFLICT" ]
}

@test "an unresolvable head ref is unknown (exit 3)" {
  run "$SCRIPT" "$REPO" main does-not-exist
  [ "$status" -eq 3 ]
  [ "$output" = "unknown" ]
}

@test "an unresolvable base ref is unknown (exit 3)" {
  run "$SCRIPT" "$REPO" origin/missing feat
  [ "$status" -eq 3 ]
  [ "$output" = "unknown" ]
}

@test "missing arguments is a usage error (exit 2)" {
  run "$SCRIPT" "$REPO" main
  [ "$status" -eq 2 ]
}
