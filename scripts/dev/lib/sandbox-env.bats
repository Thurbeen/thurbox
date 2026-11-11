#!/usr/bin/env bats
#
# The sandbox teardown stops the `fleet up` thurbox started, found by the pid in
# its log's name. A log can outlive its manager, and the pid can then belong to
# anything: the teardown must signal only a process that is a `fleet up`.

bats_require_minimum_version 1.5.0

setup() {
  export REPO_ROOT="${BATS_TEST_DIRNAME}/../../.."
  # shellcheck source=scripts/dev/lib/sandbox-env.sh
  # shellcheck disable=SC1091
  . "${BATS_TEST_DIRNAME}/sandbox-env.sh"
  ROOT="${BATS_TEST_TMPDIR}/root"
  export TBX_SANDBOX_ROOT="$ROOT"
  mkdir -p "$ROOT/data"
}

teardown() {
  [ -n "${OTHER:-}" ] && kill "$OTHER" 2>/dev/null || true
}

@test "a stale log whose pid now names another process leaves that process alone" {
  sleep 30 &
  OTHER=$!
  : > "$ROOT/data/fleet-up-$OTHER.log"
  _tbx_stop_fleet "$ROOT"
  kill -0 "$OTHER"
}

@test "a log naming a running fleet up stops it" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  # The manager's own argv, as thurbox starts it: `<fleet> up`.
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="$ROOT/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  : > "$ROOT/data/fleet-up-$OTHER.log"
  _tbx_stop_fleet "$ROOT"
  sleep 0.2
  run ! kill -0 "$OTHER"
}

@test "another sandbox's fleet up whose pid a stale log names is left alone" {
  [ -r /proc/self/environ ] || skip "no /proc to read a process's environment"
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="${BATS_TEST_TMPDIR}/other/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  : > "$ROOT/data/fleet-up-$OTHER.log"
  _tbx_stop_fleet "$ROOT"
  kill -0 "$OTHER"
}

@test "a profile made before 3.0 gets a Fleet config the new fleet reads" {
  mkdir -p "$ROOT/fleet"
  printf 'version = 1\n\n[starter]\nworkspace = "starter"\n' > "$ROOT/fleet/config.toml"
  tbx_sandbox_fleet
  run ! grep -q '^\[starter\]' "$ROOT/fleet/config.toml"
  grep -qx '\[hosts.local\]' "$ROOT/fleet/config.toml"
}

@test "an old runtime-mux config is written again with the sandbox's root" {
  mkdir -p "$ROOT/fleet"
  printf "[workspace]\nroot = '/elsewhere'\n" > "$ROOT/fleet/runtime-mux.toml"
  tbx_sandbox_fleet
  grep -qF "root = '$ROOT/fleet/workspaces'" "$ROOT/fleet/runtime-mux.toml"
}

@test "a rewrite replaces each config in one step, never truncating it in place" {
  mkdir -p "$ROOT/fleet"
  printf 'old\n' > "$ROOT/fleet/config.toml"
  printf 'old\n' > "$ROOT/fleet/runtime-mux.toml"
  before_fleet=$(stat -c %i "$ROOT/fleet/config.toml")
  before_mux=$(stat -c %i "$ROOT/fleet/runtime-mux.toml")
  tbx_sandbox_fleet
  [ "$(stat -c %i "$ROOT/fleet/config.toml")" != "$before_fleet" ]
  [ "$(stat -c %i "$ROOT/fleet/runtime-mux.toml")" != "$before_mux" ]
  run ! ls "$ROOT/fleet/"*.tmp
}
