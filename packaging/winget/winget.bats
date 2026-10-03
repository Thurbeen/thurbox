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

# Since 2026-08 a community auto-updater opens most thurbox PRs on winget-pkgs.
# Its open PR is the same queue entry as ours, so it must block too; the
# workflow lists PRs from every author for exactly this (v2.41.8 tried to stack
# on its open #445600 because the query only asked for the token's own).
@test "decide: an open PR from another account blocks a second submission" {
  prs='[{"number":445600,"state":"OPEN","createdAt":"2026-10-02T09:04:42Z","title":"Update version: Thurbeen.thurbox version 2.41.7","author":{"login":"a-community-bot"}},
        {"number":406494,"state":"MERGED","createdAt":"2026-07-23T14:10:14Z","title":"Thurbeen.thurbox version 1.1.1","author":{"login":"the-token-account"}}]'
  run decide 0 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
  echo "$output" | jq -e '.reason | test("#445600")'
}

# The title search is all that finds these PRs, and anyone can open a PR on
# winget-pkgs whose title says Thurbeen.thurbox. Only one that changes the
# package's own manifests is a queue entry for it.
@test "decide: an open PR that touches no thurbox manifest does not block" {
  prs='[{"number":7,"state":"OPEN","createdAt":"2026-09-09T11:00:00Z","title":"Thurbeen.thurbox is great","files":[{"path":"manifests/s/Some/Other/1.0/Some.Other.yaml"}]},
        {"number":8,"state":"MERGED","createdAt":"2026-09-01T12:00:00Z","title":"Thurbeen.thurbox version 2.19.0","files":[{"path":"manifests/t/Thurbeen/thurbox/2.19.0/Thurbeen.thurbox.yaml"}]}]'
  run decide 0 "$prs"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .should_submit)" = "true" ]
  echo "$output" | jq -e '.reason | test("8.0 days ago")'
}

@test "decide: an open PR that does touch a thurbox manifest blocks" {
  prs='[{"number":9,"state":"OPEN","createdAt":"2026-09-09T11:00:00Z","title":"Update version: Thurbeen.thurbox version 2.41.7","files":[{"path":"manifests/t/Thurbeen/thurbox/2.41.7/Thurbeen.thurbox.installer.yaml"}]}]'
  run decide 0 "$prs"
  [ "$(echo "$output" | jq -r .should_submit)" = "false" ]
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

# `after-sync` decides what a failed `gh repo sync` of the token account's fork
# means. Every release from 2026-09-10 to v2.41.8 died here: winget-pkgs had
# started changing `.github/workflows`, GitHub refuses to move a ref across such
# a change for a token without the `workflow` scope, and the step then ran
# `--force`, which hits the same check, and hid the reason behind a generic
# error.
after_sync() { # <ahead-by> <exit-code> <sync output>
  echo "$3" | python3 "${DIR}/submit-decision.py" after-sync --ahead "$1" --exit-code "$2"
}

@test "after-sync: a clean sync is done" {
  run after_sync 0 0 "✓ Synced the \"the-account:master\" branch from \"microsoft:master\""
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .action)" = "done" ]
}

# Run 36989062882's exact message.
@test "after-sync: a token without the workflow scope fails with the fix, not a reset" {
  msg='Upstream commits contain workflow changes, which require the `workflow` scope or permission to merge. To request it, run: gh auth refresh -s workflow'
  run after_sync 0 1 "$msg"
  [ "$status" -eq 0 ]
  [ "$(echo "$output" | jq -r .action)" = "fail" ]
  echo "$output" | jq -e '.reason | test("WINGET_TOKEN") and test("workflow")'
}

@test "after-sync: the workflow-scope refusal is never reset away, even on a diverged fork" {
  msg='Upstream commits contain workflow changes, which require the `workflow` scope or permission to merge.'
  run after_sync 3 1 "$msg"
  [ "$(echo "$output" | jq -r .action)" = "fail" ]
}

# A fork with nothing of its own can always fast-forward, so a refusal there is
# never divergence and a reset could not fix it.
@test "after-sync: a fork with no commits of its own is not reset" {
  run after_sync 0 1 "HTTP 422: something else"
  [ "$(echo "$output" | jq -r .action)" = "fail" ]
}

@test "after-sync: only a diverged fork is reset" {
  run after_sync 2 1 "HTTP 409: There are merge conflicts"
  [ "$(echo "$output" | jq -r .action)" = "force" ]
}

@test "after-sync: an unknown divergence is not reset" {
  run after_sync -1 1 "HTTP 409: There are merge conflicts"
  [ "$(echo "$output" | jq -r .action)" = "fail" ]
}

# sync-fork.ps1, the release job's fork-sync step, run under PowerShell against
# a fake `gh` that answers from FAKE_* variables and logs every call, so the
# reset path — backup branch, then `--force` — is exercised without a fork.
# CI's runner ships pwsh; a machine without it skips, CI does not.
sync_fork() {
  if ! command -v pwsh >/dev/null; then
    [ -z "${CI:-}" ] || { echo "pwsh is missing on CI" >&2; return 1; }
    skip "pwsh is not installed"
  fi
  bin="${BATS_TEST_TMPDIR}/bin"
  mkdir -p "$bin"
  cat > "${bin}/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >> "$GH_LOG"
case "$*" in
  "api user --jq .login") echo the-account ;;
  "repo view the-account/winget-pkgs "*) echo "${FAKE_FORK_BRANCH:-master}" ;;
  "repo view microsoft/winget-pkgs "*) echo "${FAKE_UPSTREAM_BRANCH:-master}" ;;
  "repo sync "*"--force"*)
    [ "${FAKE_FORCE:-ok}" = ok ] && echo "✓ reset" || { echo "HTTP 422: reset refused" >&2; exit 1; } ;;
  "repo sync "*)
    case "${FAKE_SYNC:-ok}" in
      ok) echo "✓ Synced" ;;
      scope) printf '%s\n%s\n' 'Upstream commits contain workflow changes, which require the `workflow` scope or permission to merge.' 'To request it, run: gh auth refresh -s workflow' >&2; exit 1 ;;
      *) echo "HTTP 409: There are merge conflicts" >&2; exit 1 ;;
    esac ;;
  "api repos/microsoft/winget-pkgs/compare/"*) echo "${FAKE_AHEAD:-0}" ;;
  "api repos/the-account/winget-pkgs/branches/"*) echo 0123456789abcdef0123456789abcdef01234567 ;;
  "api -X POST "*)
    [ "${FAKE_POST:-ok}" = ok ] && echo '{}' || { echo "HTTP 422: Reference already exists" >&2; exit 1; } ;;
  "api repos/the-account/winget-pkgs/git/ref/heads/"*) echo "${FAKE_KEPT:-}" ;;
  *) echo "unexpected gh call: $*" >&2; exit 99 ;;
esac
GH
  printf '#!/bin/sh\nexec python3 "$@"\n' > "${bin}/python"
  chmod +x "${bin}/gh" "${bin}/python"
  export GH_LOG="${BATS_TEST_TMPDIR}/gh.log" GITHUB_STEP_SUMMARY="${BATS_TEST_TMPDIR}/summary.md"
  : > "$GH_LOG"
  PATH="${bin}:${PATH}" run pwsh -NoProfile -NonInteractive -command ". '${DIR}/sync-fork.ps1'"
}

@test "sync-fork: a clean sync exits green and resets nothing" {
  FAKE_SYNC=ok sync_fork
  [ "$status" -eq 0 ]
  ! grep -q -- --force "$GH_LOG"
}

# Run 36989062882 again, through the real step: red, the fix named in the
# annotation, and no reset attempted.
@test "sync-fork: a token without the workflow scope fails red with the fix, without a reset" {
  FAKE_SYNC=scope FAKE_AHEAD=0 sync_fork
  [ "$status" -eq 1 ]
  echo "$output" | grep -q '::error title=winget fork sync failed::.*WINGET_TOKEN.*workflow'
  # The two-line refusal stays one annotation.
  echo "$output" | grep -q 'merge.%0ATo request it'
  ! grep -q -- --force "$GH_LOG"
}

@test "sync-fork: a diverged fork is backed up, then reset" {
  FAKE_SYNC=conflict FAKE_AHEAD=2 sync_fork
  [ "$status" -eq 0 ]
  grep -q 'ref=refs/heads/sync-backup-0123456789ab' "$GH_LOG"
  [ "$(grep -n 'POST' "$GH_LOG" | cut -d: -f1)" -lt "$(grep -n -- '--force' "$GH_LOG" | cut -d: -f1)" ]
}

@test "sync-fork: a backup an earlier run left at the same head still allows the reset" {
  FAKE_SYNC=conflict FAKE_AHEAD=2 FAKE_POST=exists FAKE_KEPT=0123456789abcdef0123456789abcdef01234567 sync_fork
  [ "$status" -eq 0 ]
  grep -q -- --force "$GH_LOG"
}

@test "sync-fork: a backup branch at another commit blocks the reset" {
  FAKE_SYNC=conflict FAKE_AHEAD=2 FAKE_POST=exists FAKE_KEPT=ffffffffffffffffffffffffffffffffffffffff sync_fork
  [ "$status" -eq 1 ]
  ! grep -q -- --force "$GH_LOG"
}

@test "sync-fork: a failed reset fails red" {
  FAKE_SYNC=conflict FAKE_AHEAD=2 FAKE_FORCE=fail sync_fork
  [ "$status" -eq 1 ]
  echo "$output" | grep -q '::error title=winget fork sync failed::could not reset'
}

# The divergence check asks upstream for its own default branch rather than
# assuming the fork's name exists there.
@test "sync-fork: compares against upstream's default branch, not the fork's name for it" {
  FAKE_SYNC=conflict FAKE_AHEAD=2 FAKE_FORK_BRANCH=main FAKE_UPSTREAM_BRANCH=master sync_fork
  grep -q 'compare/master...the-account:winget-pkgs:main' "$GH_LOG"
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
