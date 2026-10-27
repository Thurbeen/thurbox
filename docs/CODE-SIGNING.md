# Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

Windows treats an unsigned executable as unknown, so every new release starts with no
reputation. On a host that enforces Microsoft Defender's attack surface reduction rules,
that blocked a freshly provisioned `thurbox-cli.exe` again on every retry
([#1576](https://github.com/Thurbeen/thurbox/issues/1576)). An Authenticode signature
carries the reputation from one release to the next. Until the project is approved, the
release workflow ships Windows binaries unsigned, exactly as before. See
[Status](#status).

## What is signed

| File | Archive | Signed |
|---|---|---|
| `thurbox.exe` | `thurbox-vX.Y.Z-x86_64-pc-windows-msvc.zip` | yes, Authenticode |
| `thurbox-cli.exe` | `thurbox-vX.Y.Z-x86_64-pc-windows-msvc.zip` | yes, Authenticode |
| Linux and macOS binaries | `.tar.gz` | no: Authenticode is Windows-only |

Only binaries built from this repository are signed. The release archive contains just
the two executables and `LICENSE`, and it holds no third-party binary.

Everything that installs thurbox on Windows reads that same zip from the GitHub Release:
`scripts/install.ps1`, `thurbox-cli update` and the TUI's self-update, the provisioning
of a Windows peer host, Chocolatey and winget. Each one gets the signed files.

## How a release is built

Releases are cut by [`.github/workflows/cd.yml`](../.github/workflows/cd.yml) on
GitHub-hosted runners, from the release tag. No one builds a release by hand. The
Windows leg of the `build` job:

1. builds `thurbox.exe` and `thurbox-cli.exe` with `cargo build --release` for
   `x86_64-pc-windows-msvc`;
2. uploads the two unsigned executables as a workflow artifact;
3. submits that artifact to SignPath through
   [`signpath/github-action-submit-signing-request`](https://github.com/SignPath/github-action-submit-signing-request)
   (pinned by commit SHA). SignPath checks that the artifact came from this
   repository's workflow, and the request waits for a manual approval;
4. checks the returned files with `Get-AuthenticodeSignature`, which must report
   `Valid`, and copies them over the unsigned ones;
5. packages the signed executables into the release zip.

The SHA-256 checksums file is computed afterwards, so it covers the signed bytes. After
the GitHub Release is published, `verify-windows-release` downloads the zip on a Windows
runner, as an installer would. It checks the zip against the published checksums, and
when signing is configured it checks that both executables carry a valid signature.

Workflow files and build scripts count as source code. They follow the same review rule
as every other change.

## Team roles

| Role | Who | What they may do |
|---|---|---|
| Committers and reviewers | Collaborators on [Thurbeen/thurbox](https://github.com/Thurbeen/thurbox) with write access | Merge pull requests. A change from anyone outside the team must be reviewed and approved by a team member before it merges. |
| Approvers | Repository administrators of [Thurbeen](https://github.com/Thurbeen) | Approve each signing request in SignPath. A release is signed only after one of them approves it. |

Every team member must use multi-factor authentication on GitHub and on SignPath.

## Privacy

This program will not transfer any information to other networked systems unless
specifically requested by the user or the person installing or operating it.

Two settings are on by default and contact GitHub: `[features] version_check`, which
fetches the latest release from `api.github.com`, and `[features] auto_update`, which
downloads that release from `github.com`. Neither sends any data about you or your
sessions. Set both to `false` in `settings.toml` to turn them off
([`docs/CONFIG.md`](CONFIG.md)). Other traffic only happens when you ask for it: SSH to
hosts you list in `hosts.toml`, an extension or plugin you install, and OpenTelemetry
export, which is off until you enable `[telemetry]`. The coding-agent CLIs that thurbox
launches have their own privacy policies.

## Status

Signing runs once the repository has all four of these settings. With none of them set,
the release ships unsigned and the workflow says so in a notice. With only some of them
set, the release fails and the error names the missing ones.

| Name | Kind | Holds |
|---|---|---|
| `SIGNPATH_API_TOKEN` | Actions secret | API token of a SignPath user with submitter rights on the project |
| `SIGNPATH_ORGANIZATION_ID` | Actions variable | SignPath organization id |
| `SIGNPATH_PROJECT_SLUG` | Actions variable | SignPath project slug |
| `SIGNPATH_SIGNING_POLICY_SLUG` | Actions variable | Signing policy slug, the release-signing policy |

`scripts/ci/signpath-config.sh` reads them, and `tests/release_signing.rs` covers both
the gate and the order of the steps.

## Verifying a download

```powershell
Get-AuthenticodeSignature .\thurbox.exe, .\thurbox-cli.exe | Format-List Status, SignerCertificate
```

`Status` should be `Valid`, and the signer should be SignPath Foundation. Releases
published before signing was enabled are unsigned. Check those against the release's
`thurbox-vX.Y.Z-checksums.txt` instead.
