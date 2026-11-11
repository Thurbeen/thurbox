#!/usr/bin/env bats
# check-rumdl-version.sh holds a local rumdl to the version CI lints with: an
# older one passes markdown (line length, blockquotes, link fragments) that CI
# then fails.

setup() {
  SCRIPT="$BATS_TEST_DIRNAME/check-rumdl-version.sh"
  STUB_DIR="$(mktemp -d)"
  PATH="$STUB_DIR:$PATH"
}

teardown() {
  rm -rf "$STUB_DIR"
}

stub_rumdl() {
  printf '#!/bin/sh\necho "rumdl %s"\n' "$1" >"$STUB_DIR/rumdl"
  chmod +x "$STUB_DIR/rumdl"
}

@test "--print reads the version pinned on the CI markdown job" {
  run "$SCRIPT" --print
  [ "$status" -eq 0 ]
  [[ "$output" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

@test "a local rumdl at the pinned version passes" {
  stub_rumdl "$("$SCRIPT" --print)"
  run "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "a local rumdl behind CI fails and names both versions and the fix" {
  stub_rumdl 0.2.4
  run "$SCRIPT"
  [ "$status" -eq 1 ]
  [[ "$output" == *"0.2.4"* ]]
  [[ "$output" == *"$("$SCRIPT" --print)"* ]]
  [[ "$output" == *"install-dev-tools.sh"* ]]
}

@test "a missing rumdl fails with the install hint" {
  run env PATH="$STUB_DIR:/usr/bin:/bin" "$SCRIPT"
  [ "$status" -eq 1 ]
  [[ "$output" == *"install-dev-tools.sh"* ]]
}
