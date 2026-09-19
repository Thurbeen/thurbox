#!/usr/bin/env bats
#
# Drives windows-vm.sh's psmux hook-status gate probes — the half of that
# harness that can be checked without a Windows VM, and the half issue #1170
# was about. Probe A asserted the pane-user-option round-trip and reported on
# its own `ok` branch whether or not the option came back, so nothing failed
# when psmux silently dropped it. These tests pin the two directions in which
# the probes must now fail, and that a failure reaches the exit status.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/windows-vm.sh"

  # The measurement from #1170, taken on Windows 11 against psmux 3.3.6: the
  # option was set on %3, and every pane — that one included — reads it back
  # empty, because psmux implements no per-pane user options.
  PSMUX_336=$'%1 \n%3 '
  # What a multiplexer that does implement them per pane answers (tmux).
  ROUND_TRIPPED=$'%1 \n%3 working'
  # What one stored at window or server scope would answer: set on %3, read
  # back on every pane, so no pane's state belongs to its own session.
  LEAKED=$'%1 working\n%3 working'
}

# Run one of the harness's helpers without provisioning a VM: windows-vm.sh
# dispatches a subcommand only when executed, so sourcing it hands back the
# helpers alone. What the helper printed is `${lines[0]}`; the last line is
# always `rc=<status> FAILS=<count>` — the exit status and the counter `bad`
# bumps, which is what the suite's verdict reads. Both are matched whole: the
# words at stake share substrings ("unknown" contains "no", "FAILS" contains
# "FAIL"), so a loose glob here would report safety these tests do not have.
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

# A stand-in for src/session/mod.rs holding the gate at $1 ("true"/"false").
# The doc comment names the function, the way this codebase's cross-references
# do, so the read has to be anchored on the definition rather than on the name.
gate_source() {
  local path="${BATS_TEST_TMPDIR}/mod-$1.rs"
  cat >"$path" <<RS
/// See [\`psmux_hook_rewrite_supported\`], which this line is not.
///     true
pub fn psmux_hook_rewrite_supported() -> bool {
    $1
}
RS
  printf '%s\n' "$path"
}

@test "the gate is read from thurbox's own switch, not restated in the harness" {
  drive psmux_hook_gate "$(gate_source false)"
  [ "${lines[0]}" = closed ]
  drive psmux_hook_gate "$(gate_source true)"
  [ "${lines[0]}" = open ]
}

@test "a gate the harness cannot read is an error, not an assumption" {
  drive psmux_hook_gate "${BATS_TEST_TMPDIR}/no-such-file.rs"
  [ "${lines[-1]}" = "rc=1 FAILS=0" ]
}

@test "the gate probes are wired to the real source tree" {
  drive psmux_hook_gate
  [[ "${lines[0]}" = closed || "${lines[0]}" = open ]]
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
  # What holds is the mailbox. No branch reports `ok` for the gate itself,
  # because the harness cannot measure every condition the gate rests on.
  [[ "${lines[0]}" == *"mailbox"* ]]
  [[ "${lines[0]}" != *"gate: both"* ]]
}

@test "a holding mailbox never reads as the whole gate being proven" {
  # The gate's third condition — claude's --settings path on Windows — has no
  # probe here, so neither verdict over a holding pair may pass in silence.
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
