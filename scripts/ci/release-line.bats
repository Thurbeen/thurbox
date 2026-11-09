#!/usr/bin/env bats
#
# Drives release-line.sh: a held tree releases nothing, a release must be on the
# major the source declares, and only the newest release of the channels' major
# reaches the package names.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/release-line.sh"

  # git exports these to hook processes, so a suite running under this
  # project's own pre-commit hook would otherwise read the real repository.
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX \
    GIT_NAMESPACE

  REPO="${BATS_TEST_TMPDIR}/repo"
  mkdir -p "$REPO/crates/thurbox/src/update"
  cd "$REPO" || exit 1
  git init -q -b main .
  git config user.name tester
  git config user.email tester@example.invalid
  git config commit.gpgsign false
  git config tag.gpgsign false
  line 2
  git add -A
  git commit -q -m "chore: init"
  git tag v2.84.0
  git tag v2.84.1
}

hold() {
  mkdir -p .github
  echo "v3 is unreleased" >.github/release-hold
}

line() {
  printf 'pub const RELEASE_LINE: u64 = %s;\n' "$1" >crates/thurbox/src/update/version_check.rs
}

@test "a release on the source's line passes" {
  run "$SCRIPT" v2.84.2 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"held=false"* ]]
  [[ "$output" == *"line=2"* ]]
}

@test "a held tree releases nothing on the source's line" {
  hold
  run "$SCRIPT" v2.89.0 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"held=true"* ]]
  [[ "$output" == *"channels=false"* ]]
}

@test "a held tree releases nothing on the next major, even from a breaking commit" {
  hold
  line 3
  run "$SCRIPT" v3.0.0 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"held=true"* ]]
  [[ "$output" == *"channels=false"* ]]
}

@test "the real tree is held while v3 is unreleased" {
  run env THURBOX_RELEASE_HOLD_FILE="${BATS_TEST_DIRNAME}/../../.github/release-hold" \
    "$SCRIPT" v2.89.0 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"held=true"* ]]
}

@test "a breaking-change bump to the next major is refused" {
  run "$SCRIPT" v3.0.0 2
  [ "$status" -eq 1 ]
  [[ "$output" == *"refusing to release v3.0.0"* ]]
  [[ "$output" == *"RELEASE_LINE"* ]]
}

@test "the next major passes once the source moves to it" {
  line 3
  run "$SCRIPT" 3.0.0 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"line=3"* ]]
}

@test "a release of an older line from a newer source is refused" {
  line 3
  run "$SCRIPT" v2.85.0 2
  [ "$status" -eq 1 ]
}

@test "the newest release of the channels' major goes to the channels" {
  run "$SCRIPT" v2.84.2 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"channels=true"* ]]
}

@test "the newest 2.x still goes to the channels after a 3.x tag exists" {
  git tag v3.0.0
  run "$SCRIPT" v2.84.2 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"channels=true"* ]]
}

@test "a 2.x behind the newest 2.x does not" {
  git tag v2.90.0
  run "$SCRIPT" v2.84.2 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"channels=false"* ]]
}

@test "another major never goes to the channels" {
  line 3
  run "$SCRIPT" v3.0.0 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"channels=false"* ]]
}

@test "a source without RELEASE_LINE is an error, not a pass" {
  : >crates/thurbox/src/update/version_check.rs
  run "$SCRIPT" v2.84.2 2
  [ "$status" -eq 1 ]
  [[ "$output" == *"no 'pub const RELEASE_LINE"* ]]
}

@test "reads the constant the real source declares" {
  run env THURBOX_RELEASE_LINE_SOURCE="${BATS_TEST_DIRNAME}/../../crates/thurbox/src/update/version_check.rs" \
    "$SCRIPT" v2.84.2 2
  [ "$status" -eq 0 ]
  [[ "$output" == *"line=2"* ]]
}
