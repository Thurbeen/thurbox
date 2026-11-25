#!/usr/bin/env bats

bats_require_minimum_version 1.5.0

setup() {
  export REPO_ROOT="${BATS_TEST_DIRNAME}/../../.."
  # shellcheck source=scripts/dev/lib/sandbox-env.sh
  # shellcheck disable=SC1091
  . "${BATS_TEST_DIRNAME}/sandbox-env.sh"
  ROOT="${BATS_TEST_TMPDIR}/root"
  export TBX_SANDBOX_ROOT="$ROOT"
  mkdir -p "$ROOT/fleet"
}

teardown() {
  [ -n "${OTHER:-}" ] && kill "$OTHER" 2>/dev/null || true
}

@test "a stale pid file naming another process leaves that process alone" {
  sleep 30 &
  OTHER=$!
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  kill -0 "$OTHER"
}

@test "a pid file naming a running fleet up stops it" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="$ROOT/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  sleep 0.2
  run ! kill -0 "$OTHER"
}

@test "another sandbox's fleet up whose pid a stale pid file names is left alone" {
  [ -r /proc/self/environ ] || skip "no /proc to read a process's environment"
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="${BATS_TEST_TMPDIR}/other/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  kill -0 "$OTHER"
}

@test "a fleet up on the sandbox's default config, found through XDG_CONFIG_HOME, is stopped" {
  [ -r /proc/self/environ ] || skip "no /proc to read a process's environment"
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin" "$ROOT/home/.config"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  env -u FLEET_CONFIG XDG_CONFIG_HOME="$ROOT/home/.config" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  sleep 0.2
  run ! kill -0 "$OTHER"
}

@test "a fleet up on a default config outside the sandbox is left alone" {
  [ -r /proc/self/environ ] || skip "no /proc to read a process's environment"
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  env -u FLEET_CONFIG XDG_CONFIG_HOME="${BATS_TEST_TMPDIR}/elsewhere/.config" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  kill -0 "$OTHER"
}

@test "the sandbox starts its own fleet up, and its teardown stops it" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  PATH="$bin:$PATH" FLEET_CONFIG="$ROOT/fleet/config.toml" tbx_sandbox_fleet_up
  OTHER=$(cat "$ROOT/fleet/up.pid")
  sleep 0.2
  kill -0 "$OTHER"
  _tbx_stop_fleet "$ROOT"
  sleep 0.2
  run ! kill -0 "$OTHER"
  [ ! -e "$ROOT/fleet/up.pid" ]
}

@test "a manager slow to stop is gone before a new one starts" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\ntrap "sleep 1; exit 0" TERM\nwhile :; do sleep 0.1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="$ROOT/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  _tbx_stop_fleet "$ROOT"
  run ! kill -0 "$OTHER"
}

@test "a manager that will not stop fails the stop instead of hanging" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  printf '#!/bin/sh\ntrap "" TERM\nwhile :; do sleep 0.1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="$ROOT/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  echo "$OTHER" > "$ROOT/fleet/up.pid"
  TBX_FLEET_STOP_TIMEOUT=1 run _tbx_stop_fleet "$ROOT"
  [ "$status" -ne 0 ]
  [ "$(cat "$ROOT/fleet/up.pid")" = "$OTHER" ]
  kill -9 "$OTHER"
}

@test "a manager an older sandbox named only in its log is stopped" {
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin" "$ROOT/thurbox-data"
  printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > "$bin/fleet"
  chmod +x "$bin/fleet"
  FLEET_CONFIG="$ROOT/fleet/config.toml" "$bin/fleet" up &
  OTHER=$!
  sleep 0.2
  : > "$ROOT/thurbox-data/fleet-up-$OTHER.log"
  _tbx_stop_fleet "$ROOT"
  run ! kill -0 "$OTHER"
}

@test "a profile made before 3.0 gets a Fleet config the new fleet reads" {
  mkdir -p "$ROOT/fleet"
  printf 'version = 1\n\n[starter]\nworkspace = "starter"\n' > "$ROOT/fleet/config.toml"
  tbx_sandbox_fleet
  run ! grep -q '^\[starter\]' "$ROOT/fleet/config.toml"
  grep -qx '\[runtimes.mux\]' "$ROOT/fleet/config.toml"
  run ! grep -q '^\[hosts' "$ROOT/fleet/config.toml"
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
  run ! ls "$ROOT/fleet/"*.toml.*
}

@test "two launches writing the configs at once both finish" {
  mkdir -p "$ROOT/fleet"
  for _ in 1 2 3 4 5 6 7 8; do
    ( tbx_sandbox_fleet ) & a=$!
    ( tbx_sandbox_fleet ) & b=$!
    wait "$a"
    wait "$b"
  done
  grep -qx '\[runtimes.mux\]' "$ROOT/fleet/config.toml"
  run ! grep -q '^\[hosts' "$ROOT/fleet/config.toml"
  run ! ls "$ROOT/fleet/"*.toml.*
}
