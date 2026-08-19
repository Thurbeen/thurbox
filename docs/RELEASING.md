# Releasing

Releases are automated (`cd.yml`, driven by cocogitto) and the mechanics are in
`CLAUDE.md`. This file is for the part automation cannot check: **what a release
may and may not change about the artifacts**, because most users never read
release notes — `[features] auto_update` defaults to `true`, so the TUI replaces
its own binaries on startup.

## The rule that matters

> **Never remove `thurbox` or `thurbox-cli` from a release archive.**

The updater in the *already installed* binary decides what to install, and it
hard-fails on a known binary that is missing from the tarball:

    // src/agent/self_update.rs
    const BINARIES: [&str; 2] = ["thurbox", "thurbox-cli"];
    // …
    if !src.exists() {
        return Err(format!("release tarball is missing `{name}`"));
    }

`run_auto_update` is best-effort — "failures are logged and swallowed" — so the
symptom is not an error anybody sees. It is **auto-update silently ceasing to
work, permanently, on every install already out there**, with the only trace in
`thurbox.log`. There is no way to push a fix to a client whose updater has
stopped running, so this mistake cannot be corrected by a later release.

Renaming what a binary *does* is fine. Renaming or dropping the file is not.

## Adding a binary is safe, and also invisible

Three properties, all verified in the code, make an added binary inert for
existing installs:

1. The updater only looks for the names in its own `BINARIES`, so an unknown
   extra file in the archive is ignored.
2. Even for a *known* name, it skips a binary that is not already on disk
   (`if !dest.exists() { skipped.push(..); continue; }`). The updater replaces
   binaries; it never introduces one.
3. Every distribution channel names binaries explicitly — Homebrew
   (`bin.install`), both AUR PKGBUILDs, winget (`NestedInstallerFiles`), and both
   installers. Only Chocolatey globs, because it auto-shims every `.exe`.

The consequence worth stating plainly: **auto-update will never deliver a new
binary to anyone.** A user acquires one by re-running an installer or their
package manager. That is a feature for an opt-in interface and a trap if you
expect a new tool to reach the existing population.

## v1 is a branch, not a binary

`thurbox` runs the plugin kernel; `src/app` and `src/ui` are gone. v1 is
maintained on the **`v1.x`** branch, and a patch is released by dispatching
`cd.yml` from that ref with an explicit `version` (e.g. `1.8.7`) — `cog bump
--auto` computes from tags and would try to move the 2.x line instead.

The archive still contains exactly `thurbox` and `thurbox-cli`, which is what the
rule at the top of this file requires. The kernel inherited the *name*; nothing was
added or removed, so no installed updater notices anything but a new version.

A profile with v1 history meets `kernel::consent` on its first launch: it is asked
once, before the interface takes the terminal, and declining turns `auto_update`
off and prints how to reinstall 1.x. That gate is the only reason replacing an
interface under an unchanged binary name is defensible.

Such a cut is a **maintenance release**, and `cd.yml` treats it as one: it
compares the version being cut against every existing tag and, when it is behind
the highest, publishes the GitHub Release with `make_latest: false` and skips
Homebrew, AUR, Chocolatey and winget. The tag, the four-platform build and the
release assets are unchanged — what is withheld is every pointer that means "the
current thurbox" rather than a version. Without it a 1.8.x cut would take the
`releases/latest` pointer the installers resolve and rewrite the tap and the
PKGBUILDs to it, walking 2.x users backwards. The test is the version, not the
ref, so a hotfix from any branch that really is the newest still ships
everywhere.

Two things had to be repaired before that dispatch could reach a tag at all, and
both are worth knowing because neither is reachable from a `main` release — they
were broken from the moment 2.x took `main` and nothing noticed, since v1.8.6 was
cut while v1 still *was* `main`. `cog.toml`'s `branch_whitelist` carries `v*.x`
beside `main`, without which `cog bump` refuses on the maintenance branch ("No
patterns matched in [main] for branch 'v1.x'") before creating anything. And the
tag is pushed by refspec rather than by `ad-m/github-push-action`, which defaults
its `branch` to the repository's default: harmless from `main`, where that is the
ref already checked out, but from `v1.x` it tries to push that branch's tip onto
`main`, is rejected as a non-fast-forward, and the tag never leaves the runner.

## Checklist for a release that changes artifacts

- [ ] `thurbox` and `thurbox-cli` are still in **both** archive steps of `cd.yml`
      (the `tar czf` line and the `Compress-Archive` line).
- [ ] A newly added binary is in both archive steps *and* in every channel:
      `packaging/homebrew/Formula/thurbox.rb`, `packaging/aur/thurbox/PKGBUILD`,
      `packaging/aur/thurbox-bin/PKGBUILD`,
      `packaging/winget/manifests/*installer.yaml`, `scripts/install.sh`,
      `scripts/install.ps1`.
- [ ] Installers handle its **absence**, so installing an older release still
      works (`scripts/install.bats` asserts this).
- [ ] Adding to `BINARIES` was a deliberate decision, not a reflex.
- [ ] Chocolatey and winget are throttled to one publish per 30 days, so those
      channels can lag a month behind GitHub Releases. Do not treat a missing
      package version as a failure.
