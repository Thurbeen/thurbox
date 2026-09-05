---
name: thurbox-release
description: Thurbox's automated release pipeline and installers: the two release gates (commit type + artifact relevance), version injection via THURBOX_RELEASE_VERSION, release artifacts, and the downstream Homebrew/AUR/Chocolatey/winget publish jobs with their 30-day throttles; plus scripts/install.sh and install.ps1 specifics. Use when changing cd.yml, versioning, packaging/, the install scripts, or when a release did or did not cut as expected.
---

# Thurbox releases, packaging and installers

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Release Process

Releases are **fully automated** via GitHub Actions. No version commits
are created - version is determined by git tags only.

### How It Works

Every push to `main` automatically triggers the release workflow:

1. **Commit Analysis**: Analyzes all commits since last tag using cocogitto
2. **Release Decision** — a release needs **both** gates to pass:
   - **Commit type** (`check-release`'s `parse` step): commits must include
     `feat`, `fix`, or `perf`. Only docs/chore/ci commits → no release.
   - **Artifact relevance** (`check-release`'s `shipped` step): the diff since
     the last tag must touch something a user installs (`src/`, `tests/`,
     `ui/`, `examples/`, `build.rs`, `Cargo.toml`/`Cargo.lock`,
     `rust-toolchain.toml`, `Cross.toml`, `extensions/`, `packaging/`,
     `scripts/install.{sh,ps1}`, `cd.yml`). `extensions/` and `examples/panes/`
     are there although no binary carries them: a bare-name install resolves
     against the release *tag*, so a change never tagged is one nobody can
     install. `ui/` and `examples/lua/` are there more directly still — both are
     `include_str!`d into the binary, so they are bytes a user installs.
     Commit type alone over-releases: Renovate labels a GitHub-Actions pin bump
     `fix(deps)` and the website is versioned `feat(ui)`/`fix(ui)`, so a
     CSS-only or lint-action-only change used to cut a real release (v1.2.13
     through v1.3.0 were four website-only releases, one a *minor*) — burning a
     4-platform build and pushing to the moderated Chocolatey/winget channels
     for a no-op binary. Such commits stay in history and ride along in the
     next real release's changelog.
   - The gate is evaluated over the **whole span since the last tag**, not the
     single push, so a website-only push landing on an unreleased `src/` commit
     still cuts the release it owes. A forced `workflow_dispatch` version
     **skips** the relevance gate — an explicit human cut is always honoured.
3. **Automated Release** (if needed):
   - Determines semantic version (feat→minor, fix/perf→patch, breaking→major)
   - Creates lightweight git tag: `v{version}` (e.g., v1.0.0)
   - Pushes tag to origin
   - Builds binaries for 4 platforms (3 Unix `.tar.gz` + 1 Windows `.zip`;
     version passed via environment variable)
   - Generates changelog from commits
   - Publishes GitHub Release with binaries and release notes

### Version Management

- **Cargo.toml version**: Always `0.0.0-dev` (static development marker)
- **Real version**: Determined by release workflow (v1.0.0, v1.1.0, etc.)
- **Build-time injection**: `build.rs` uses `THURBOX_RELEASE_VERSION` environment
  variable (set by workflow) to inject version into binary
- **Development builds**: Show `0.0.0-dev` (when `THURBOX_RELEASE_VERSION` not set)
- **Release builds**: Show actual version (e.g., `1.0.0`) via env variable from workflow
- **Explicit cuts**: `cog bump --auto` computes the next version from commits and
  works for every ordinary release (at 1.x+, a breaking change correctly bumps
  the major). To cut a *specific* version that `--auto` can't reach — the only
  way across a major boundary from a `0.x` line, or any one-off — dispatch the
  Release workflow (`cd.yml`) with the `version` input (e.g. `1.0.0`); it runs
  `cog bump --version <v>` instead of `--auto`.

### Release Artifacts

Each release includes:

- Binaries for 4 platforms:
  - `thurbox-v{ver}-x86_64-unknown-linux-gnu.tar.gz`
  - `thurbox-v{ver}-x86_64-unknown-linux-musl.tar.gz`
  - `thurbox-v{ver}-aarch64-apple-darwin.tar.gz`
  - `thurbox-v{ver}-x86_64-pc-windows-msvc.zip` (the Windows artifact
    extracted by `install.ps1` / packaged by Chocolatey + winget)
- `thurbox-v{ver}-checksums.txt` (SHA256 sums for verification)
- Changelog with categorized commits

### Distribution Packages

After the GitHub Release is published, `cd.yml` also updates the downstream
package channels (each gated on its secret, skipped on forks):

- **Homebrew** (`publish-homebrew`): bumps `version`/`sha256` in
  `packaging/homebrew/Formula/thurbox.rb` (via `packaging/homebrew/bump-formula.py`,
  reading the release `checksums.txt`) and pushes it to the
  `Thurbeen/homebrew-thurbox` tap over SSH. Needs the `HOMEBREW_TAP_DEPLOY_KEY`
  secret (a write deploy key on the tap repo; the org blocks cross-repo PATs).
  Install: `brew install thurbeen/thurbox/thurbox`. Supports macOS arm64
  (`aarch64-apple-darwin`) + Linux x86_64 (`x86_64-unknown-linux-musl`).
- **AUR** (`publish-aur`): bumps + pushes `thurbox`/`thurbox-bin` PKGBUILDs.
  Needs `AUR_SSH_PRIVATE_KEY`.
- **Chocolatey** (`publish-chocolatey`): bumps `<version>` in
  `packaging/chocolatey/thurbox.nuspec` and `$url64`/`$checksum64` in
  `tools/chocolateyinstall.ps1` (via `packaging/chocolatey/bump-nuspec.py`,
  reading the release `checksums.txt`), then `choco pack` + `choco push` to the
  community repo. Runs on `windows-latest`; needs the `CHOCOLATEY_API_KEY`
  secret. New versions go through community-repo moderation.
  Install: `choco install thurbox`. Windows x86_64 only.
  **Throttled to one push per `THROTTLE_DAYS` (30d) window** because the
  community repo moderates + rate-limits every push and can't keep up with
  thurbox's per-`feat`/`fix`/`perf` cadence (versions pile up → `choco push`
  returns **403**). The job queries the community OData feed
  (`community.chocolatey.org/api/v2/Packages()`) for the last-published
  version's age; younger than the window ⇒ **skip the push, exit green** with a
  `::warning::` (patch releases coalesce into the next monthly Chocolatey
  version — the binary still ships immediately via GitHub Releases +
  Homebrew/AUR/winget). A residual `403`/`409` at push time is likewise caught
  and exits green; only a genuine failure (bad package, auth) fails red — so a
  backed-up channel never turns the whole release red.
- **winget** (`publish-winget`): bumps `PackageVersion`/`InstallerUrl`/
  `InstallerSha256`/`ReleaseNotesUrl` in the three manifests under
  `packaging/winget/manifests/` (via `packaging/winget/bump-manifests.py`,
  reading the release `checksums.txt`), then `wingetcreate submit`s the set as a
  PR to `microsoft/winget-pkgs`. Runs on `windows-latest`; needs the
  `WINGET_TOKEN` secret (a `public_repo` PAT owning a fork of
  `microsoft/winget-pkgs`). New versions go through winget-pkgs PR
  validation + review.
  **Throttled to one submission per `THROTTLE_DAYS` (30d) window**, mirroring
  Chocolatey, because winget-pkgs is *manually moderated* — each `submit` opens a
  PR a human must review, so thurbox's per-`feat`/`fix`/`perf` cadence buried the
  maintainers (30 open PRs at once, flagged in
  [microsoft/winget-pkgs#405639](https://github.com/microsoft/winget-pkgs/pull/405639)).
  The throttle step ages our own last thurbox PR (via `gh pr list`, merged or
  open) — the winget analog of the choco feed's Published date; younger than the
  window ⇒ **skip the submission, exit green** with a `::warning::` (they
  coalesce into the next monthly PR). As second-line cleanup for a PR that still
  stacks (e.g. a manual dispatch inside the window), a follow-up `gh pr close`
  closes every older still-open `Thurbeen.thurbox` PR from the token account
  (wingetcreate's `--replace` only supersedes a *published* manifest version,
  not a pending PR; best-effort, never fails the release).
  The release zip is a `zip` installer with
  `NestedInstallerType = portable` (PATH aliases `thurbox`/`thurbox-cli`, no
  MSI). Install: `winget install Thurbeen.thurbox`. Windows x86_64 only.

See `packaging/README.md` for the full packaging overview.

### Commit Types and Versioning

Thurbox 1.0+ follows [Semantic Versioning](https://semver.org/):

- **feat**: Minor version bump (1.x.0)
- **fix, perf**: Patch version bump (1.0.x)
- **docs, chore, ci, style, test**: No release (appear in next version)
- **BREAKING CHANGE**: Major version bump (x.0.0)

A breaking change bumps the major version automatically via `cog bump --auto`
(at 1.x+; on a `0.x` line cocogitto maps breaking to a *minor* bump instead, so
the only way to cross into 1.0 was the explicit-version Release dispatch).


## Installation Script

**Linux / macOS** — `scripts/install.sh`:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows** — `scripts/install.ps1` (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

Both installers share the same shape: ASCII banner, platform detection, version
resolution (GitHub API → releases-page scrape fallback), SHA256 checksum
verification, extract, post-install hints. From the same release, `install.sh`
pulls the `.tar.gz` for `x86_64-unknown-linux-musl` / `aarch64-apple-darwin`
(Linux x86_64 + Apple-silicon macOS — the only platforms it installs onto; it
errors cleanly on any other), while `install.ps1` pulls
**`thurbox-<ver>-x86_64-pc-windows-msvc.zip`** (the Windows artifact built by
`cd.yml`) and extracts it with the built-in `Expand-Archive` (no tar needed).
ARM64 Windows installs the x86_64 build (runs under x64 emulation).

**`install.sh` (POSIX `sh`) specifics:**

- Colorized output (auto-disabled when stderr is not a TTY, `NO_COLOR` is set,
  or `TERM=dumb`); platforms Linux/macOS × x86_64/aarch64
- No external deps beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Env vars: `VERSION=v1.0.0`, `INSTALL_DIR=/path` (default `~/.local/bin`)
- Non-interactive (safe pipe-to-shell), cleanup via `trap`
- Tested by `scripts/install.bats` (bats-core, ~28 tests; CI `install-script` job)

**`install.ps1` (PowerShell 5.1+) specifics:**

- Parameters `-Version` / `-InstallDir` / `-Repo`, or the matching
  `THURBOX_VERSION` / `THURBOX_INSTALL_DIR` / `THURBOX_REPO` env vars (env vars
  are the reliable path for the `irm | iex` form, which can't pass parameters);
  default install dir `%LOCALAPPDATA%\Programs\thurbox`
- Adds the install dir to the **user** `PATH` (`[Environment]::SetEnvironmentVariable(... 'User')`)
  when missing; reflects it into the current session
- ASCII-only source (no BOM needed; survives `irm | iex` decoding on Windows
  PowerShell 5.1); `Write-Host` for UI is intentional (`Write-Output` would leak
  into the `iex` pipeline)
- Pure helpers (`Get-Target`, `Get-ExpectedChecksum`) are guarded by
  `$env:THURBOX_PS_TEST` so the file can be dot-sourced for testing without
  running the installer
- Tested by `scripts/install.Tests.ps1` (Pester 5; CI `install-script-ps` job,
  run with `pwsh` on ubuntu since the helpers are platform-independent) —
  the PowerShell mirror of `install.bats`

