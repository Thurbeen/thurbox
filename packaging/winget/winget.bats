#!/usr/bin/env bats
#
# The winget channel's two decisions, exercised without cutting a release:
# whether to submit at all (submit-decision.py `decide`), and whether a failed
# `wingetcreate submit` is the moderated channel pushing back or a real break
# (`classify`). Plus bump-manifests.py against a recorded `checksums.txt`.

setup() {
  DIR="${BATS_TEST_DIRNAME}"
}

# `--now` is fixed so the age arithmetic is deterministic; the PR fixtures are
# dated relative to it.
NOW="2026-09-09T12:00:00Z"

decide() { # <throttle-days> <prs-json>
  echo "$2" | python3 "${DIR}/submit-decision.py" decide --throttle-days "$1" --now "$NOW"
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

# The cadence ask: at THROTTLE_DAYS=0 every release attempts, exactly like
# Chocolatey — the last submission's age can never gate it.
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

# `classify` decides green-with-a-warning vs red. The moderated-channel shapes
# are deferrable; anything else must stay visible.
@test "classify: a GitHub rate limit is deferrable" {
  run bash -c "echo 'API rate limit exceeded for user ID 1234.' | python3 '${DIR}/submit-decision.py' classify"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .deferrable)" = "true" ]
}

@test "classify: an already-submitted version is deferrable" {
  run bash -c "echo 'A pull request for this version has already been submitted.' | python3 '${DIR}/submit-decision.py' classify"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .deferrable)" = "true" ]
}

# Run 34381951096's exact failure. Now that the job syncs the fork before
# submitting, seeing this again means the sync did not work — that must stay a
# red job, not a warning nobody reads.
@test "classify: the stale-fork failure is NOT deferrable" {
  msg='The forked repository could not be synced with the upstream commits. Sync your fork manually and try again.'
  run bash -c "echo '$msg' | python3 '${DIR}/submit-decision.py' classify"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .deferrable)" = "false" ]
}

@test "classify: a manifest validation failure is NOT deferrable" {
  run bash -c "echo 'Manifest validation failed: InstallerSha256 mismatch' | python3 '${DIR}/submit-decision.py' classify"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .deferrable)" = "false" ]
}

# bump-manifests.py against a recorded release checksums.txt.
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
