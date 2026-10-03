# The release job's "Sync the winget-pkgs fork" step (.github/workflows/cd.yml),
# out of the workflow so winget.bats can run it against a fake `gh`. Brings the
# WINGET_TOKEN account's fork of microsoft/winget-pkgs up to date before
# `wingetcreate submit`; how a failed sync is read is `submit-decision.py
# after-sync`, and the reasons are in the step's comment. Needs GH_TOKEN and
# GITHUB_STEP_SUMMARY.
$ErrorActionPreference = "Continue"
$account = (gh api user --jq .login | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or -not $account) {
  Write-Host "::warning title=winget fork sync skipped::could not resolve the WINGET_TOKEN account; leaving the fork to wingetcreate."
  exit 0
}
# A workflow command ends at the first newline, so gh's output is
# escaped the way the runner unescapes it before it goes in one.
function Escape-Annotation([string]$text) {
  $text.Trim() -replace '%', '%25' -replace "`r", '%0D' -replace "`n", '%0A'
}
$fork = "$account/winget-pkgs"
$branch = (gh repo view $fork --json defaultBranchRef --jq .defaultBranchRef.name | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or -not $branch) {
  Write-Host "No $fork yet - wingetcreate forks on first submit."
  exit 0
}
$out = gh repo sync $fork --source microsoft/winget-pkgs 2>&1 | Out-String
$syncExit = $LASTEXITCODE
Write-Host $out
# Commits on the fork's default branch that upstream lacks: 0 means it
# can always fast-forward, -1 that it could not be told. Upstream is asked
# for its own default branch, since a fork's can be renamed.
$ahead = 0
if ($syncExit -ne 0) {
  $upstream = (gh repo view microsoft/winget-pkgs --json defaultBranchRef --jq .defaultBranchRef.name | Out-String).Trim()
  $ahead = -1
  if ($LASTEXITCODE -eq 0 -and $upstream) {
    $ahead = (gh api "repos/microsoft/winget-pkgs/compare/$($upstream)...$($account):winget-pkgs:$($branch)" --jq .ahead_by | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $ahead -notmatch '^\d+$') { $ahead = -1 }
  }
}
$verdict = $out |
  python (Join-Path $PSScriptRoot submit-decision.py) after-sync --ahead $ahead --exit-code $syncExit |
  ConvertFrom-Json
if ($null -eq $verdict) {
  Write-Error "could not classify the fork sync (gh repo sync exited $syncExit)."
  exit 1
}
if ($verdict.action -eq "done") {
  Write-Host "Synced $fork from microsoft/winget-pkgs."
  exit 0
}
if ($verdict.action -eq "fail") {
  Write-Host "::error title=winget fork sync failed::could not sync $fork with microsoft/winget-pkgs: $($verdict.reason). gh said: $(Escape-Annotation $out)"
  "winget fork sync **failed**: $($verdict.reason)." | Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Append
  exit 1
}
$head = (gh api "repos/$fork/branches/$branch" --jq .commit.sha | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') {
  Write-Host "::error title=winget fork sync failed::$fork has diverged ($($verdict.reason)) but its head could not be read, so it is not reset."
  exit 1
}
$backup = "sync-backup-$($head.Substring(0, 12))"
gh api -X POST "repos/$fork/git/refs" -f "ref=refs/heads/$backup" -f "sha=$head" | Out-Null
$backedUp = $LASTEXITCODE -eq 0
if (-not $backedUp) {
  # Already there from an earlier run whose reset then failed: the
  # head is just as kept, so the reset may go ahead.
  $kept = (gh api "repos/$fork/git/ref/heads/$backup" --jq .object.sha | Out-String).Trim()
  $backedUp = $LASTEXITCODE -eq 0 -and $kept -eq $head
}
if (-not $backedUp) {
  Write-Host "::error title=winget fork sync failed::$fork has diverged ($($verdict.reason)) but its head could not be backed up to $backup, so it is not reset."
  exit 1
}
Write-Host "::warning title=winget fork reset::$fork diverged ($($verdict.reason)); its old head is kept on branch $backup and $branch is reset to microsoft/winget-pkgs."
$out = gh repo sync $fork --source microsoft/winget-pkgs --force 2>&1 | Out-String
$syncExit = $LASTEXITCODE
Write-Host $out
if ($syncExit -ne 0) {
  Write-Host "::error title=winget fork sync failed::could not reset $fork to microsoft/winget-pkgs. gh said: $(Escape-Annotation $out)"
  exit 1
}
exit 0
