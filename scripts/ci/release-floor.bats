#!/usr/bin/env bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/release-floor.sh"

  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX \
    GIT_NAMESPACE

  REPO="${BATS_TEST_TMPDIR}/repo"
  mkdir -p "$REPO"
  cd "$REPO" || exit 1
  git init -q -b main .
  git config user.name tester
  git config user.email tester@example.invalid
  git config commit.gpgsign false
  git config tag.gpgsign false
  git commit -q --allow-empty -m "chore: init"
  git tag v2.83.6
}

@test "a version above every tag of its major is kept" {
  run "$SCRIPT" v2.83.7
  [ "$status" -eq 0 ]
  [ "$output" = "v2.83.7" ]
}

@test "a version behind an unreleased higher tag is raised to the next minor" {
  git tag v2.84.0
  git tag v2.88.0
  run "$SCRIPT" v2.83.7
  [ "$status" -eq 0 ]
  [ "$output" = "v2.89.0" ]
}

@test "a version equal to the highest tag is raised too" {
  git tag v2.88.0
  run "$SCRIPT" v2.88.0
  [ "$status" -eq 0 ]
  [ "$output" = "v2.89.0" ]
}

@test "the next release after the floor follows cog again" {
  git tag v2.88.0
  git tag v2.89.0
  run "$SCRIPT" v2.89.1
  [ "$status" -eq 0 ]
  [ "$output" = "v2.89.1" ]
}

@test "tags of another major do not raise it" {
  git tag v3.0.0
  run "$SCRIPT" v2.83.7
  [ "$status" -eq 0 ]
  [ "$output" = "v2.83.7" ]
}

@test "a version without a readable major is an error" {
  run "$SCRIPT" main
  [ "$status" -ne 0 ]
}
