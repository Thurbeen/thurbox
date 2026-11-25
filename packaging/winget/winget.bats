#!/usr/bin/env bats

setup() {
  DIR="${BATS_TEST_DIRNAME}"
}

FIXTURE_NOW="2026-09-09T12:00:00Z"

decide() {
  local throttle_days="$1" prs_json="$2"
  echo "$prs_json" | python3 "${DIR}/submit-decision.py" decide --throttle-days "$throttle_days" --now "$FIXTURE_NOW"
}

@test "decide: no prior PR is a first submission" {
  run decide 30 '[]'
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "true" ]
  echo "$output" | jq -e '.reason | test("first submission")'
}

@test "decide: an open thurbox PR blocks a second submission" {
  prs='[{"number":405639,"state":"OPEN","createdAt":"2026-09-01T12:00:00Z","title":"New version: Thurbeen.thurbox version 2.19.0"}]'
  run decide 0 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
  echo "$output" | jq -e '.reason | test("#405639")'
}

@test "decide: an open PR blocks even when the throttle window has elapsed" {
  prs='[{"number":1,"state":"OPEN","createdAt":"2026-01-01T12:00:00Z","title":"New version: Thurbeen.thurbox version 2.0.0"}]'
  run decide 30 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
}

@test "decide: a merged PR inside the window is throttled" {
  prs='[{"number":2,"state":"MERGED","createdAt":"2026-09-04T12:00:00Z","title":"New version: Thurbeen.thurbox version 2.19.0"}]'
  run decide 30 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
  echo "$output" | jq -e '.reason | test("5.0 days ago")'
}

@test "decide: throttle 0 submits however recent the last merged PR is" {
  prs='[{"number":3,"state":"MERGED","createdAt":"2026-09-09T11:00:00Z","title":"New version: Thurbeen.thurbox version 2.19.5"}]'
  run decide 0 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "true" ]
}

@test "decide: a closed PR does not block, only ages the channel" {
  prs='[{"number":4,"state":"CLOSED","createdAt":"2026-07-01T12:00:00Z","title":"New version: Thurbeen.thurbox version 2.10.0"}]'
  run decide 30 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "true" ]
}

@test "decide: the newest PR is the one that ages the channel" {
  prs='[{"number":5,"state":"MERGED","createdAt":"2026-01-01T12:00:00Z","title":"old"},
        {"number":6,"state":"MERGED","createdAt":"2026-09-08T12:00:00Z","title":"new"}]'
  run decide 30 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
  echo "$output" | jq -e '.reason | test("1.0 days ago")'
}

@test "decide: rejects a throttle that is not a number" {
  run bash -c "echo '[]' | python3 '${DIR}/submit-decision.py' decide --throttle-days abc"
  [ "$status" -ne 0 ]
}

after_submit() {
  local exit_code="$1" submit_output="$2"
  echo "$submit_output" | python3 "${DIR}/submit-decision.py" after-submit --exit-code "$exit_code"
}

@test "after-submit: a successful submit opened a PR" {
  run after_submit 0 "Pull request created: https://github.com/microsoft/winget-pkgs/pull/999"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .opened)" = "true" ]
  [ "$(echo "$output" | jq -r .fail)" = "false" ]
}

@test "after-submit: a deferred submit reports it opened nothing" {
  run after_submit 1 "API rate limit exceeded for user ID 1234."
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .opened)" = "false" ]
  [ "$(echo "$output" | jq -r .deferrable)" = "true" ]
  [ "$(echo "$output" | jq -r .fail)" = "false" ]
}

@test "after-submit: an already-submitted version is deferrable and opened nothing" {
  run after_submit 1 "A pull request for this version has already been submitted."
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .opened)" = "false" ]
  [ "$(echo "$output" | jq -r .deferrable)" = "true" ]
}

@test "after-submit: the stale-fork failure fails the job" {
  msg='The forked repository could not be synced with the upstream commits. Sync your fork manually and try again.'
  run after_submit 1 "$msg"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .opened)" = "false" ]
  [ "$(echo "$output" | jq -r .deferrable)" = "false" ]
  [ "$(echo "$output" | jq -r .fail)" = "true" ]
}

@test "after-submit: a manifest validation failure fails the job" {
  run after_submit 1 "Manifest validation failed: InstallerSha256 mismatch"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .fail)" = "true" ]
  [ "$(echo "$output" | jq -r .opened)" = "false" ]
}

@test "after-submit: opened and fail are never both true" {
  for code_and_output in "0|created" "1|API rate limit exceeded" "1|Manifest validation failed"; do
    code="${code_and_output%%|*}"
    text="${code_and_output#*|}"
    result="$(after_submit "$code" "$text")"
    [ "$(echo "$result" | jq -r '.opened and .fail')" = "false" ]
  done
}

@test "bump-manifests: rewrites all three manifests from recorded checksums" {
  cp -r "${DIR}/manifests" "${BATS_TEST_TMPDIR}/manifests"
  run python3 "${DIR}/bump-manifests.py" v2.19.6 "${BATS_TEST_TMPDIR}/manifests" \
    "${DIR}/testdata/checksums-v2.19.6.txt"
  [ "$status" -eq 0 ]
  grep -q "^PackageVersion: 2.19.6$" "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.yaml"
  grep -q "^PackageVersion: 2.19.6$" "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.installer.yaml"
  grep -q "^PackageVersion: 2.19.6$" "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.locale.en-US.yaml"
  # winget-pkgs validation normalizes the digest to uppercase.
  grep -q "InstallerSha256: 93E3FDE8F2E16F50C9D4B23DFC0A257FC3461B82BAD454BAB352195016A40308" \
    "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.installer.yaml"
  grep -q "InstallerUrl:.*v2.19.6/thurbox-v2.19.6-x86_64-pc-windows-msvc.zip" \
    "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.installer.yaml"
  grep -q "^ReleaseNotesUrl: .*releases/tag/v2.19.6$" \
    "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.locale.en-US.yaml"
}

@test "bump-manifests: fails loudly when the Windows checksum is missing" {
  cp -r "${DIR}/manifests" "${BATS_TEST_TMPDIR}/manifests"
  grep -v windows "${DIR}/testdata/checksums-v2.19.6.txt" > "${BATS_TEST_TMPDIR}/checksums.txt"
  run python3 "${DIR}/bump-manifests.py" v2.19.6 "${BATS_TEST_TMPDIR}/manifests" \
    "${BATS_TEST_TMPDIR}/checksums.txt"
  [ "$status" -ne 0 ]
}
