#!/usr/bin/env bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/windows-vm.sh"

  PSMUX_336=$'%1 \n%3 '
  ROUND_TRIPPED=$'%1 \n%3 working'
  LEAKED=$'%1 working\n%3 working'
}

drive() {
  run bash -c '
    . "$1" || exit 99
    set +e
    shift
    FAILS=0
    "$@"
    printf "rc=%s FAILS=%s\n" "$?" "$FAILS"
  ' _ "$SCRIPT" "$@"
}

source_declaring() {
  local path="${BATS_TEST_TMPDIR}/psmux-$1.rs" other=true
  [ "$1" = true ] && other=false
  cat >"$path" <<RS
impl TmuxCompatible for Psmux {
    /// \`HOOK_STATUS: bool = $other\` in a doc comment is not the constant.
    const HOOK_STATUS: bool = $1;
}
RS
  printf '%s\n' "$path"
}

@test "the gate is what runtime-mux's psmux adapter declares, not restated in the harness" {
  drive psmux_hook_gate "$(source_declaring false)"
  [ "${lines[0]}" = closed ]
  drive psmux_hook_gate "$(source_declaring true)"
  [ "${lines[0]}" = open ]
}

@test "the real psmux adapter's gate is readable" {
  drive psmux_hook_gate
  [[ "${lines[0]}" == open || "${lines[0]}" == closed ]]
}

@test "a gate the harness cannot read is an error, not an assumption" {
  drive psmux_hook_gate "${BATS_TEST_TMPDIR}/no-such-file.rs"
  [ "${lines[-1]}" = "rc=1 FAILS=0" ]
}

@test "a source declaring no HOOK_STATUS is unreadable, not closed" {
  local path="${BATS_TEST_TMPDIR}/silent.rs"
  printf 'impl TmuxCompatible for Psmux {}\n' >"$path"
  drive psmux_hook_gate "$path"
  [ "${lines[-1]}" = "rc=1 FAILS=0" ]
}

@test "the psmux 3.3.6 answer reads as unsupported, not as a pass" {
  drive pane_option_measured '%3' working "$PSMUX_336"
  [ "${lines[0]}" = no ]
}

@test "a round-tripped option reads as supported" {
  drive pane_option_measured '%3' working "$ROUND_TRIPPED"
  [ "${lines[0]}" = yes ]
}

@test "an option every pane can read is not the per-pane mailbox" {
  drive pane_option_measured '%3' working "$LEAKED"
  [ "${lines[0]}" = no ]
}

@test "nothing at all is unknown rather than unsupported" {
  drive pane_option_measured '%3' working ""
  [ "${lines[0]}" = unknown ]
}

@test "an open gate over an option psmux drops fails the suite" {
  drive psmux_gate_verdict open no yes
  [ "${lines[-1]}" = "rc=0 FAILS=1" ]
}

@test "a closed gate over an option psmux drops is recorded, not failed" {
  drive psmux_gate_verdict closed no no
  [ "${lines[-1]}" = "rc=0 FAILS=0" ]
  [[ "${lines[0]}" == *1170* ]]
}

@test "a closed gate over a psmux that implements both halves must be reconsidered" {
  drive psmux_gate_verdict closed yes yes
  [ "${lines[-1]}" = "rc=0 FAILS=1" ]
}

@test "an open gate over a psmux that implements both halves passes" {
  drive psmux_gate_verdict open yes yes
  [ "${lines[-1]}" = "rc=0 FAILS=0" ]
  [[ "${lines[0]}" == *"mailbox"* ]]
  [[ "${lines[0]}" != *"gate: both"* ]]
}

@test "a holding mailbox never reads as the whole gate being proven" {
  drive psmux_gate_verdict open yes yes
  [[ "$output" == *"--settings"* ]]
  [[ "$output" == *"not probed here"* ]]
  drive psmux_gate_verdict closed yes yes
  [[ "$output" == *"--settings"* ]]
}

@test "a pair that does not hold says nothing about the unprobed condition" {
  drive psmux_gate_verdict closed no no
  [[ "$output" != *"--settings"* ]]
}

@test "a probe that could not be measured fails rather than passing quietly" {
  drive psmux_gate_verdict closed unknown no
  [ "${lines[-1]}" = "rc=0 FAILS=1" ]
  drive psmux_gate_verdict open yes unknown
  [ "${lines[-1]}" = "rc=0 FAILS=1" ]
}

@test "a failed gate probe fails the whole smoke test" {
  drive smoke_verdict smoke 1
  [ "${lines[-1]}" = "rc=1 FAILS=0" ]
  [[ "${lines[0]}" == *FAIL* ]]
}

@test "the smoke test still passes when every probe agrees with the gate" {
  drive smoke_verdict smoke 0
  [ "${lines[-1]}" = "rc=0 FAILS=0" ]
  [[ "${lines[0]}" == *PASS* ]]
}

@test "a session that did not round-trip still fails, gate probes aside" {
  drive smoke_verdict "" 0
  [ "${lines[-1]}" = "rc=1 FAILS=0" ]
}
