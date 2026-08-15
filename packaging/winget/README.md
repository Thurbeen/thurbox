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
> `winget install Thurbeen.thurbox` resolves. Each *new version* still goes
> through PR review there, and submissions are throttled to one per 30 days (see
> [Automated publishing](#automated-publishing-ci) below) — so the winget
> channel trails the newest release. For the latest build immediately, use
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
release cadence, so **both are throttled to one submission per 30 days** (see
[Automated publishing](#automated-publishing-ci)); the newest binary always
ships immediately via GitHub Releases regardless.

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
`windows-latest` after the GitHub Release is created and, **when the throttle
window has elapsed** (below):

1. downloads the release `thurbox-<version>-checksums.txt`,
2. runs [`bump-manifests.py`](bump-manifests.py) to set `PackageVersion` across
   the manifests and the installer manifest's `InstallerUrl`/`InstallerSha256`
   (uppercased, as winget-pkgs expects) plus the locale `ReleaseNotesUrl` from
   those checksums,
3. downloads `wingetcreate` (`https://aka.ms/wingetcreate/latest`),
4. `wingetcreate submit`s the manifest set, which validates it and opens a PR
   against [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs),
   then
5. closes any *older* still-open `Thurbeen.thurbox` PR from the token account,
   keeping only the one just opened (second-line cleanup behind the throttle).

The job needs a `WINGET_TOKEN` secret — a classic PAT with the `public_repo`
scope on the account that owns a fork of `microsoft/winget-pkgs` (wingetcreate
pushes the manifest branch to that fork and opens the PR). The job is skipped
where the secret is absent (e.g. on forks). The committed template files are not
modified by CI — they stay as last-known-good, exactly like the Chocolatey /
Homebrew templates.

> **Throttled to one submission per `THROTTLE_DAYS` (30 days)**, mirroring
> Chocolatey. winget-pkgs is a *manually moderated* repo — each `submit` opens a
> PR a human must review — so it can't absorb thurbox's per-`feat`/`fix`/`perf`
> release cadence: a release every few hours buries the maintainers under stale
> version-bump PRs (30 open at once, flagged in
> [microsoft/winget-pkgs#405639](https://github.com/microsoft/winget-pkgs/pull/405639)).
> So the job first reads our own last thurbox PR on winget-pkgs (via `gh pr list`,
> merged or open) for its age — the winget analog of the Chocolatey feed's
> Published date. If it is younger than `THROTTLE_DAYS` the job **skips the
> submission and exits green** with a `::warning::`, coalescing the intervening
> patch releases into the next monthly winget PR. The binary itself always ships
> immediately via GitHub Releases (and Homebrew/AUR); only the winget channel
> lags. Tune the cadence via the `THROTTLE_DAYS` env in the job. When the window
> *has* elapsed and a submission goes out, the follow-up cleanup step closes any
> older still-open PR (best-effort, never fails the release) — since
> `wingetcreate` has no supersede-pending-PR mode (`--replace` only affects a
> *published* manifest version).
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
