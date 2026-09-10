# winget packaging

Thurbox ships a [winget](https://learn.microsoft.com/windows/package-manager/)
package that installs the **prebuilt** x86_64 Windows release binaries
(`thurbox.exe` + `thurbox-cli.exe`) from the GitHub Release as **portable**
commands on your `PATH`.

```powershell
winget install Thurbeen.thurbox
```

> **Status: live.** The package is published on
> [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs), so
> `winget install Thurbeen.thurbox` resolves. Every release attempts a
> submission (see [Automated publishing](#automated-publishing-ci) below), but
> each *new version* goes through PR review there and only one thurbox PR is in
> flight at a time — so the winget channel trails the newest release by however
> long that review takes. For the latest build immediately, use
> [`scripts/install.ps1`](../../scripts/install.ps1)
> (`irm … | iex`) or the GitHub Release zip.

The canonical manifest set lives here under [`manifests/`](manifests/):

| File | Manifest type | Purpose |
| ---- | ------------- | ------- |
| [`Thurbeen.thurbox.yaml`](manifests/Thurbeen.thurbox.yaml) | `version` | ties the version to the locale + installer manifests |
| [`Thurbeen.thurbox.installer.yaml`](manifests/Thurbeen.thurbox.installer.yaml) | `installer` | the release zip URL + SHA256 + nested portable exes |
| [`Thurbeen.thurbox.locale.en-US.yaml`](manifests/Thurbeen.thurbox.locale.en-US.yaml) | `defaultLocale` | descriptive metadata (publisher, license, tags, description) |

The `PackageVersion`/`InstallerUrl`/`InstallerSha256`/`ReleaseNotesUrl` values
committed here are a last-known-good template — CI overrides them per release.

## Why winget as well as Chocolatey

winget is Microsoft's first-party Windows package manager, bundled with Windows
10/11 via *App Installer* — so a Windows user can `winget install
Thurbeen.thurbox` with nothing else installed, whereas Chocolatey must be set up
first. Both are published from the same release and neither replaces the other.
Both are also *manually moderated* channels that can't keep pace with thurbox's
release cadence. winget **attempts every release and backs off when the
channel pushes back** (see [Automated publishing](#automated-publishing-ci));
Chocolatey is throttled to one publish per 30 days instead. The newest binary
always ships immediately via GitHub Releases regardless.

## Supported platforms

Windows x86_64 only — the single published Windows release artifact is
`thurbox-v<version>-x86_64-pc-windows-msvc.zip`. ARM64 Windows installs the
x86_64 build and runs it under x64 emulation (matching
[`scripts/install.ps1`](../../scripts/install.ps1)).

The installer is a `zip` whose `NestedInstallerType` is `portable`: winget
extracts the archive and registers PATH aliases (`thurbox`, `thurbox-cli`) — no
MSI, no per-machine installer, and `winget uninstall Thurbeen.thurbox` removes
them cleanly.

## Runtime dependencies

winget manifests have no cross-package dependency mechanism for this, so these
are documented in the package `Description` rather than auto-installed:

- **[psmux](https://github.com/psmux/psmux)** — the native-Windows terminal
  multiplexer thurbox drives (a drop-in tmux clone). Install it separately.
- A coding-agent CLI (claude, codex, antigravity, opencode, aider, …) on your
  PATH.

## Automated publishing (CI)

Every release submits to winget-pkgs **automatically** — including the first.
The `publish-winget` job in
[`.github/workflows/cd.yml`](../../.github/workflows/cd.yml) runs on
`windows-latest` after the GitHub Release is created and, **when
[`submit-decision.py`](submit-decision.py) says to submit** (below):

1. downloads the release `thurbox-<version>-checksums.txt`,
2. runs [`bump-manifests.py`](bump-manifests.py) to set `PackageVersion` across
   the manifests and the installer manifest's `InstallerUrl`/`InstallerSha256`
   (uppercased, as winget-pkgs expects) plus the locale `ReleaseNotesUrl` from
   those checksums,
3. downloads `wingetcreate` (`https://aka.ms/wingetcreate/latest`),
4. syncs the token account's `winget-pkgs` fork from upstream (below),
5. `wingetcreate submit`s the manifest set, which validates it and opens a PR
   against [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs),
   then
6. closes any *older* still-open `Thurbeen.thurbox` PR from the token account,
   keeping only the one just opened (second-line cleanup).

The job needs a `WINGET_TOKEN` secret — a classic PAT with the `public_repo`
scope on the account that owns a fork of `microsoft/winget-pkgs` (wingetcreate
pushes the manifest branch to that fork and opens the PR). The job is skipped
where the secret is absent (e.g. on forks). The committed template files are not
modified by CI — they stay as last-known-good, exactly like the Chocolatey /
Homebrew templates.

> **Cadence: every release, paced by the queue rather than a calendar.**
> winget-pkgs is a *manually moderated* repo — each `submit` opens a PR a human
> must review — and thurbox's per-`feat`/`fix`/`perf` cadence can bury the
> maintainers under stale version-bump PRs (30 open at once, flagged in
> [microsoft/winget-pkgs#405639](https://github.com/microsoft/winget-pkgs/pull/405639)).
> The rule that prevents that is **one thurbox PR in flight**, not a monthly
> window: the job lists our own thurbox PRs on winget-pkgs (`gh pr list`, any
> state) and hands them to [`submit-decision.py`](submit-decision.py), which
> **skips the submission and exits green** with a `::warning::` while one is
> still open — `wingetcreate` cannot update a pending PR, so a second one would
> only lengthen the queue. The next release retries; the binary itself always
> ships immediately via GitHub Releases (and Homebrew/AUR), so only the winget
> channel lags.
>
> `THROTTLE_DAYS` survives as a knob on top of that, now **defaulted to `0`** —
> no calendar throttle, so winget ships as often as Chocolatey attempts to. Set
> it to `30` in the job to restore the old monthly window without a code change:
> a submission younger than that many days also skips green.
>
> **Fork sync.** `wingetcreate submit` pushes the manifest branch to the token
> account's fork of winget-pkgs and its own auto-sync of that fork is
> fast-forward only. winget-pkgs merges hundreds of PRs a day, so a fork touched
> rarely falls thousands of commits behind and the submit dies with *"The forked
> repository could not be synced with the upstream commits"* — which is what
> failed v2.19.6. The job therefore runs `gh repo sync <account>/winget-pkgs
> --source microsoft/winget-pkgs` first, retrying with `--force` (a hard reset of
> the fork's default branch) when it cannot fast-forward. That is safe here
> because the fork is a submission staging area: nothing of ours lives on its
> default branch and every submission gets its own branch, so a reset destroys
> neither work nor an open PR.
>
> **When `submit` fails.** A rejection from the channel itself (GitHub rate
> limit, version already pending) warns and exits green — the same shape as the
> Chocolatey push's 403/409 handling, with the classification in
> `submit-decision.py classify`. Anything else fails the job, but the job carries
> `continue-on-error: true`, so a broken winget channel can never turn the
> Release run red once the binaries are on GitHub Releases.
>
> **Tested without cutting a release.** `bats packaging/winget/winget.bats`
> (CI job *winget Packaging Tests*, or `just test-scripts`) runs
> `bump-manifests.py` against a recorded `checksums.txt` and pins every
> submit/skip and deferrable/red decision — including that the stale-fork
> message is *not* treated as deferrable, since the sync step exists to prevent
> it. The Windows-only halves (`wingetcreate`, `gh repo sync` against a real
> diverged fork) are not covered.
>
> **Review (winget-pkgs side, not CI).** microsoft/winget-pkgs runs automated
> validation (manifest schema, installer hash, a sandbox install/uninstall
> smoke test) and then human review before a version goes live. The
> `wingetcreate submit` succeeds when the PR is opened; the package appears in
> `winget search thurbox` only after that PR merges. This is not a CI failure.

## Manual publishing / initial import

Publishing is automated (above), so this is only a fallback — e.g. to re-submit
a version outside the release flow. On Windows with
[wingetcreate](https://github.com/microsoft/winget-create) installed
(`winget install wingetcreate`):

```powershell
# Bump the local template to a published release.
$ver = "<version>"   # a tag with published release assets, e.g. 0.79.46
# curl.exe (not the `curl` alias for Invoke-WebRequest) ships on Windows 10+.
curl.exe -fsSL -o checksums.txt `
  "https://github.com/Thurbeen/thurbox/releases/download/v$ver/thurbox-v$ver-checksums.txt"
python packaging\winget\bump-manifests.py "v$ver" packaging\winget\manifests checksums.txt

# Validate, then submit a PR to microsoft/winget-pkgs.
wingetcreate submit --token <your-github-pat> packaging\winget\manifests
```

Pick a `<version>` that has **published release assets** (the manifest points at
a release zip). To only sanity-check the manifests without submitting, use
`winget validate --manifest packaging\winget\manifests`.
