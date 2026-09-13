#!/usr/bin/env bats
#
# The winget channel's two decisions, exercised without cutting a release:
# whether to submit at all (submit-decision.py `decide`), and whether a
# completed `wingetcreate submit` opened a PR, was pushed back by the
# moderated channel, or failed for real (`after-submit`). Plus
# bump-manifests.py against a recorded `checksums.txt`.

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

# `after-submit` turns a finished `wingetcreate submit` into the three things the
# workflow needs: did it open a PR (`opened`, which gates the cleanup step), may
# the job exit green (`deferrable`), must it fail (`fail`).
after_submit() { # <exit-code> <submit output>
  echo "$2" | python3 "${DIR}/submit-decision.py" after-submit --exit-code "$1"
}

@test "after-submit: a successful submit opened a PR" {
  run after_submit 0 "Pull request created: https://github.com/microsoft/winget-pkgs/pull/999"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .opened)" = "true" ]
  [ "$(echo "$output" | jq -r .fail)" = "false" ]
}

# The regression this file exists to prevent. A deferred submission exits green
# having opened NOTHING, so cleanup must not run: closing the pending PR behind
# a submission that never happened leaves winget-pkgs with no thurbox PR at all
# and the version silently never ships.
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

# Run 34381951096's exact failure. Now that the job syncs the fork before
# submitting, seeing this again means the sync did not work — that must stay a
# red job, not a warning nobody reads.
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

# No path may report both: cleanup keys off `opened`, and a job that failed
# must never look like it opened a PR.
@test "after-submit: opened and fail are never both true" {
  for code_and_output in "0|created" "1|API rate limit exceeded" "1|Manifest validation failed"; do
    code="${code_and_output%%|*}"
    text="${code_and_output#*|}"
    result="$(after_submit "$code" "$text")"
    [ "$(echo "$result" | jq -r '.opened and .fail')" = "false" ]
  done
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

# `after-sync` reads a finished `gh repo sync` of the token account's fork and
# says whether it synced, is worth retrying with `--force`, or cannot succeed.
after_sync() { # <exit-code> <sync output>
  echo "$2" | python3 "${DIR}/submit-decision.py" after-sync --exit-code "$1"
}

@test "after-sync: a clean sync needs nothing more" {
  run after_sync 0 "✓ Synced the \"LeTuR:master\" branch from \"microsoft:master\""
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .synced)" = "true" ]
  [ "$(echo "$output" | jq -r .fail)" = "false" ]
}

@test "after-sync: a diverged fork is retried with --force" {
  run after_sync 1 "can't sync because there are diverging changes; use \`--force\` to overwrite the destination branch"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .retry_force)" = "true" ]
  [ "$(echo "$output" | jq -r .fail)" = "false" ]
}

# Run 34749835769's exact failure, and every release's from 34541900941 on. The
# token lacks the `workflow` scope, so GitHub refuses to move the fork onto
# upstream commits that touch .github/workflows — with or without `--force`,
# which updates the same ref. Retrying cannot help; the job must say which
# scope to add instead of blaming a stale fork.
@test "after-sync: a token without the workflow scope fails with the fix, no --force" {
  msg='Upstream commits contain workflow changes, which require the `workflow` scope or permission to merge. To request it, run: gh auth refresh -s workflow'
  run after_sync 1 "$msg"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .synced)" = "false" ]
  [ "$(echo "$output" | jq -r .retry_force)" = "false" ]
  [ "$(echo "$output" | jq -r .fail)" = "true" ]
  echo "$output" | jq -e '.reason | test("WINGET_TOKEN") and test("`workflow` scope")'
}

# The raw API wording, for a gh that stops translating it.
@test "after-sync: the API's own workflow-scope refusal is recognised too" {
  run after_sync 1 'refusing to allow a Personal Access Token to create or update workflow `.github/workflows/x.yaml` without `workflow` scope'
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .fail)" = "true" ]
  [ "$(echo "$output" | jq -r .retry_force)" = "false" ]
}

# The safety rule is "no second thurbox PR on the moderation queue", whoever
# opened the first. A third-party bot (damn-good-b0t) submits thurbox too —
# every version merged since 1.8.6 is its — so a decision fed only the token
# account's PRs would stack a second PR on top of the bot's.
@test "decide: the workflow asks about thurbox PRs from every author" {
  step="$(awk '/name: Decide whether to submit/{f=1} f&&/- name: Download release checksums/{exit} f' \
    "${DIR}/../../.github/workflows/cd.yml")"
  echo "$step" | grep -q 'gh pr list --repo microsoft/winget-pkgs'
  ! echo "$step" | grep -q -- '--author'
}

# Both binaries import VCRUNTIME140.dll (the MSVC target links the CRT
# dynamically), so a machine without the VC++ 2015+ runtime cannot start them.
# winget installs a declared dependency first; winget-pkgs' merged manifests
# carry it, and ours must too or the next submission drops it.
@test "manifest: the installer declares the VC++ runtime the binaries import" {
  grep -q "PackageIdentifier: Microsoft.VCRedist.2015+.x64" \
    "${DIR}/manifests/Thurbeen.thurbox.installer.yaml"
  cp -r "${DIR}/manifests" "${BATS_TEST_TMPDIR}/manifests"
  python3 "${DIR}/bump-manifests.py" v2.19.6 "${BATS_TEST_TMPDIR}/manifests" \
    "${DIR}/testdata/checksums-v2.19.6.txt"
  grep -q "PackageIdentifier: Microsoft.VCRedist.2015+.x64" \
    "${BATS_TEST_TMPDIR}/manifests/Thurbeen.thurbox.installer.yaml"
}

@test "bump-manifests: fails loudly when the Windows checksum is missing" {
  cp -r "${DIR}/manifests" "${BATS_TEST_TMPDIR}/manifests"
  grep -v windows "${DIR}/testdata/checksums-v2.19.6.txt" > "${BATS_TEST_TMPDIR}/checksums.txt"
  run python3 "${DIR}/bump-manifests.py" v2.19.6 "${BATS_TEST_TMPDIR}/manifests" \
    "${BATS_TEST_TMPDIR}/checksums.txt"
  [ "$status" -ne 0 ]
}
