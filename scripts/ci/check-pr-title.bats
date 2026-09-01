#!/usr/bin/env bats
#
# Drives check-pr-title.sh: what a pull request title must be for squash merge
# to land a commit `cog` can still parse, and the mistakes it exists to catch.

setup() {
  CHECKER="${BATS_TEST_DIRNAME}/check-pr-title.sh"
  if ! command -v cog >/dev/null 2>&1; then
    skip "cocogitto (cog) is not installed"
  fi

  # git exports these to hook processes, so a suite running under this
  # project's own pre-commit hook would otherwise read the real repository
  # instead of the temporary one below.
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX \
    GIT_NAMESPACE

  REPO="${BATS_TEST_TMPDIR}/repo"
  mkdir -p "$REPO"
  cd "$REPO" || exit 1
  git init -q -b main .
  git config user.name tester
  git config user.email tester@example.invalid
  # cog.toml is read by `cog verify` from the working directory, so the scope
  # allowlist a repository declares is part of what the checker enforces.
  printf 'scopes = ["core", "cli"]\n' >cog.toml
}

@test "accepts a conventional title" {
  run "$CHECKER" "fix(core): keep the caret where the frame put it" 1044
  [ "$status" -eq 0 ]
}

@test "validates the title in the form squash merge lands, suffix included" {
  run "$CHECKER" "fix(core): keep the caret where the frame put it" 1044
  [ "$status" -eq 0 ]
  [[ "$output" == *"(#1044)"* ]]
}

@test "rejects a title that is not a conventional commit" {
  run "$CHECKER" "Fix the caret" 1044
  [ "$status" -eq 1 ]
  [[ "$output" == *"not a conventional commit"* ]]
}

@test "rejects a commit type cog.toml does not declare" {
  run "$CHECKER" "wibble(core): keep the caret where the frame put it" 1044
  [ "$status" -eq 1 ]
}

@test "rejects a scope outside the allowlist" {
  run "$CHECKER" "fix(lint): keep the caret where the frame put it" 1044
  [ "$status" -eq 1 ]
}

@test "accepts a breaking change declared in the title" {
  run "$CHECKER" "feat(core)!: drop the v1 pane API" 1050
  [ "$status" -eq 0 ]
}

@test "rejects a title that already ends in its own (#N)" {
  run "$CHECKER" "fix(core): keep the caret where the frame put it (#1044)" 1044
  [ "$status" -eq 1 ]
  [[ "$output" == *"already ends in"* ]]
}

@test "accepts a title citing another pull request mid-sentence" {
  run "$CHECKER" "fix(core): finish what (#900) started for the caret" 1044
  [ "$status" -eq 0 ]
}

@test "rejects an empty title" {
  run "$CHECKER" "" 1044
  [ "$status" -eq 1 ]
  [[ "$output" == *"empty"* ]]
}

@test "reports a usage error when the pr number is missing" {
  run "$CHECKER" "fix(core): keep the caret where the frame put it"
  [ "$status" -eq 2 ]
}

@test "works in a checkout that has configured no git identity" {
  # `cog verify` resolves the current author before parsing and panics when
  # user.name is unset, which is every fresh CI checkout. The checker lends one
  # through a throwaway HOME; without that this call aborts.
  git config --unset user.name
  git config --unset user.email
  HOME="${BATS_TEST_TMPDIR}/empty-home"
  mkdir -p "$HOME"
  run env HOME="$HOME" "$CHECKER" "fix(core): keep the caret" 1044
  [ "$status" -eq 0 ]
}
