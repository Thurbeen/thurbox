#!/usr/bin/env bats
#
# Drives check-conventional-commits.sh over throwaway repositories: what it
# accepts, what it rejects, and the one subject it is here to exempt.

setup() {
  CHECKER="${BATS_TEST_DIRNAME}/check-conventional-commits.sh"
  if ! command -v cog >/dev/null 2>&1; then
    skip "cocogitto (cog) is not installed"
  fi

  # git exports these to hook processes, so a suite running under this
  # project's own pre-commit hook would otherwise commit into the real
  # repository instead of the temporary one below.
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX \
    GIT_NAMESPACE

  REPO="${BATS_TEST_TMPDIR}/repo"
  mkdir -p "$REPO"
  cd "$REPO"
  git init -q -b main .
  git config user.name tester
  git config user.email tester@example.invalid
  # cog.toml is read by `cog verify`, so the scope allowlist a repository
  # declares is part of what the checker enforces.
  printf 'scopes = ["cli"]\n' >cog.toml
  commit "chore: seed"
}

# Commit an empty change carrying $1 as its message.
commit() {
  git commit -q --allow-empty -m "$1"
}

@test "a history of conventional commits passes" {
  commit "feat(cli): add a flag"
  commit "fix: correct the flag"
  run "$CHECKER"
  [ "$status" -eq 0 ]
}

@test "a non-conventional commit fails and is named" {
  commit "just fixing things"
  local sha
  sha=$(git rev-parse HEAD)
  run "$CHECKER"
  [ "$status" -eq 1 ]
  [[ "$output" == *"$sha"* ]]
  [[ "$output" == *"just fixing things"* ]]
}

@test "the gate's own CI-fix commit is exempt" {
  commit "no-mistakes: apply CI fixes"
  run "$CHECKER"
  [ "$status" -eq 0 ]
}

@test "the gate's own agent-fix commit is exempt" {
  commit "no-mistakes: apply agent fixes"
  run "$CHECKER"
  [ "$status" -eq 0 ]
}

@test "a message that merely quotes the exempt subject is still checked" {
  commit "revert of no-mistakes: apply CI fixes"
  run "$CHECKER"
  [ "$status" -eq 1 ]
}

@test "cog.toml's scope allowlist is still enforced" {
  commit "fix(bogus): a scope this repository does not declare"
  run "$CHECKER"
  [ "$status" -eq 1 ]
}

@test "a merge commit's message is skipped" {
  git checkout -q -b side
  commit "feat(cli): work on a branch"
  git checkout -q main
  git merge -q --no-ff -m "Merge branch 'side'" side
  run "$CHECKER"
  [ "$status" -eq 0 ]
}

@test "an explicit range limits what is checked" {
  commit "just fixing things"
  local base
  base=$(git rev-parse HEAD)
  commit "feat(cli): after the bad one"
  run "$CHECKER" "${base}..HEAD"
  [ "$status" -eq 0 ]
}
