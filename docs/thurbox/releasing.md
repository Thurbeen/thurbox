# Releasing

Releases are automated (`cd.yml`, driven by cocogitto) and the mechanics are in
the `thurbox-release` skill. This file is for the part automation cannot check: **what a release
may and may not change about the artifacts**, because most users never read
release notes — `[features] auto_update` defaults to `true`, so the TUI replaces
its own binaries on startup.

## The rule that matters

> **Never remove `thurbox` or `thurbox-cli` from a release archive.**

The updater in the *already installed* binary decides what to install, and it
hard-fails on a known binary that is missing from the tarball:

    // src/update/self_update.rs
    const BINARIES: [&str; 2] = ["thurbox", "thurbox-cli"];
    // …
    if !src.exists() {
        return Err(format!("release archive is missing `{name}`"));
    }

The startup auto-update (`kernel::updates`) runs `perform_update` on a thread
and only logs a failure (`auto-update failed: …`), so the
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
unsupported and its code stays on the **`v1.x`** branch. Its last patches were
released by dispatching `cd.yml` from that ref with an explicit `version` (e.g. `1.8.7`) — `cog bump
--auto` computes from tags and would try to move the 2.x line instead.

The archive still contains exactly `thurbox` and `thurbox-cli`, which is what the
rule at the top of this file requires. The kernel inherited the *name*; nothing was
added or removed, so no installed updater notices anything but a new version.

A profile with v1 history meets `kernel::consent` on its first launch: it is asked
once, before the interface takes the terminal, and declining turns `auto_update`
off and prints how to reinstall 1.x. That gate is the only reason replacing an
interface under an unchanged binary name is defensible.

That off-switch is reversible, and only by the gate that threw it. Declining
records a marker in `metadata` alongside the `settings.toml` write, and a later
**accept** puts `auto_update` back only when that marker is present — so someone
who declined once and returned to v2 is not left pinned forever, while an
`auto_update = false` the *user* set is never overridden. The marker is the whole
mechanism: without it the two cases are indistinguishable on disk, and the accept
branch has to choose between overturning a preference and stranding a profile.
It is taken in one `DELETE … RETURNING`, so it acts once.

### Auto-update does not cross a major, and that has to be shipped to v1

From v2.2.3, `update::version_check::crosses_major` stopped `perform_update`
installing a release whose major is higher than the running binary's; it was
reported instead (`UpdateOutcome::SkippedMajor`, `thurbox-cli update --force` to
take it anyway). It has since become the stricter line rule in "v2 and v3"
below, which no flag crosses. Either way, this is what makes "stay on 1.x" a
property of the binary rather than a setting the user has to remember.

**It only protects a binary that carries it.** Every release up to v1.8.7 predates
this commit, so those installs still resolve `releases/latest` to a 2.x tag and
take it. Closing that needs a **1.x maintenance cut carrying this change**,
released the way this section describes — dispatch `cd.yml` from `v1.x` with an
explicit `version`, which publishes with `make_latest: false` and skips the
package channels. Until such a cut exists, the only reliable hold for an existing
1.x install is `auto_update = false`, which is what the consent gate writes.

Note the ordering trap when you do cut it: the guard reads the running binary's
own major, so it is the *1.x* build that must contain it. Landing it on `main`
alone protects 2.x from a future 3.x and nobody else.

The other way to protect those installs was to move GitHub's `releases/latest`
pointer back to the newest 1.x tag, which every pre-guard binary and every
installer resolves. **That was considered and rejected: 2.x stays `latest`.** It
would have frozen auto-update for the whole 2.x population — a 2.x binary
resolving a 1.x tag sees nothing newer and stops updating — and made the
unpinned one-liner install 1.x, which is a different claim than "1.x is
recommended". The cost is that the guard reaches existing 1.x installs only via
the maintenance cut above, and until then `auto_update = false` is what holds
them.

Such a cut is a **maintenance release**, and `cd.yml` treats it as one: it
compares the version being cut against every existing tag and, when it is behind
the highest, publishes the GitHub Release with `make_latest: false` and skips
Homebrew, AUR, Chocolatey and winget. The tag, the five-target build and the
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

## v2 and v3

3.x (issue #1688: Fleet as thurbox's engine, one repository) is a different
program under the same binary names, as 2.x was to 1.x. **No 2.x install moves
to 3.x unless its user reinstalls on purpose.** Everything below exists to keep
that true for installs whose update code can no longer change.

### What every shipped updater does

Every updater from v1.0.0 to v2.84.x resolves GitHub's `releases/latest`, then
downloads `releases/download/v<V>/thurbox-v<V>-checksums.txt` with `curl -fsSL`
(or `wget`) **before** the archive, and returns an error — replacing nothing —
when that download fails. They differ only in what they do with a newer major:

| Installed | On a 3.x "Latest" |
|---|---|
| v1.0.0 – v1.8.7, v2.0.0 – v2.2.2 | no major guard: would install 3.x |
| v2.2.3 – v2.84.x | `crosses_major`: report 3.x, install nothing, and never see a later 2.x either |
| this change onwards | the line rule below: install the newest 2.x, report 3.x |

### The line rule (v2 binaries from this change)

`update::version_check::RELEASE_LINE` is the major this source tree releases.
The update check, the startup auto-update and `thurbox-cli update` read GitHub's
release **list** and only consider releases on that line, so a 2.x maintenance
release cut after 3.0.0 — published with `make_latest: false` — is still found
and installed. A newer major is reported (`thurbox-cli version --check`,
`thurbox-cli update`, a log line on startup) with a link here, and never
installed: `--force` reinstalls the newest 2.x, nothing else. The header badge
only offers the line. **Ship this in a 2.x release before any 3.x release
exists**: the more installs carry it, the fewer depend on the asset rule below.

### What a 3.x release must do

1. **Never publish an asset named `thurbox-v<V>-checksums.txt`** (name the sums
   file anything else, for example `thurbox-<V>-sha256sums.txt`). That one name is
   what every pre-line-rule updater downloads first, so its absence makes all of
   them fail safe — logged as `auto-update failed`, binaries untouched — whatever
   `releases/latest` points at. This is the only protection the v1.x and
   v2.0.0–v2.2.2 installs have, and it is a property of the 3.x release, not of
   any code they carry.
2. **"Latest"**: when 3.0.0 becomes GitHub's "Latest", fresh one-liner installs
   get 3.x, and v2.2.3–v2.84.x installs stop auto-updating (they see 3.x, skip
   it, and never look for a 2.x) until their user runs a 2.x installer once. That
   is safe but stale. Keeping "Latest" on 2.x for a while instead keeps those
   installs on 2.x patches; then the 3.x installer must resolve the newest 3.x tag
   itself rather than `releases/latest`.
3. **Package channels**: the existing names — Homebrew `thurbox`, AUR `thurbox`
   and `thurbox-bin`, Chocolatey `thurbox`, winget `Thurbeen.thurbox` — carry 2.x
   for good, so `brew upgrade`, an AUR helper, `choco upgrade` or `winget upgrade`
   never moves anyone across. 3.x ships under names of its own, chosen when it
   ships. `cd.yml` enforces the first half: `PACKAGE_CHANNELS_MAJOR` is `2`, and
   only the newest 2.x tag is pushed to those names (`scripts/ci/release-line.sh`),
   including a 2.x maintenance release cut after 3.x exists.
4. **Installers**: `scripts/install.{sh,ps1}` install `releases/latest` unless
   given a version, so a 2.x user re-running the one-liner gets whatever is
   "Latest". `VERSION=v2.x.y` (`THURBOX_VERSION` on Windows) pins 2.x. A Nix
   flake input that follows `main` builds 3.x on its next `nix flake update`;
   point it at `v2.x` or a 2.x tag.
5. **State**: 3.x must not write anything a 2.x install reads, so both can sit on
   one machine and 2.x keeps working. Give 3.x its own config directory, data
   directory and tmux socket, and have it import 2.x state read-only. 2.x
   already refuses rather than corrupts a database whose `min_reader_version` is
   above its schema (`storage::schema::MIN_READER_VERSION`), but refusing is not
   working: sharing `~/.local/share/thurbox/thurbox.db` would end 2.x the day 3.x
   raised it. The same applies to the `thurbox` tmux socket and the agent hook
   entries that call `thurbox-cli`.

### Cutting the major, and 2.x after it

`cog bump --auto` turns a `BREAKING CHANGE:` footer or a `feat!:` into a new
major, and a squash merge can carry a branch commit's body onto `main`. So
`cd.yml` runs `scripts/ci/release-line.sh` before tagging and **refuses any
version whose major is not `RELEASE_LINE`** — on a push or a dispatch alike. If a
breaking footer slips onto `main`, the Release run fails instead of tagging
3.0.0; cut the 2.x it owed by dispatching with an explicit `version`.

3.0.0 therefore takes two deliberate steps: a reviewed pull request that sets
`RELEASE_LINE = 3` (and does what the list above asks), then a dispatch of the
Release workflow with `version: 3.0.0`. Before that pull request lands, branch
`v2.x` from the last 2.x commit; `cog.toml` already allows `v*.x`. A 2.x
maintenance release is cut by dispatching `cd.yml` from `v2.x` with an explicit
version: it is tagged and built, published without "Latest" while a 3.x tag is
higher, and still pushed to the 2.x package names as the newest 2.x.

### The v3 hold

`main` carries 3.x work before 3.0.0 is cut. A `feat` or `fix` on it would
otherwise cut a 2.x release that ships that work to every 2.x user and channel,
and the line rule does not stop that: the version is still on line 2.

So the tree holds its releases. While `.github/release-hold` exists,
`scripts/ci/release-line.sh` answers `held=true` and `cd.yml` tags and
publishes nothing, on a push or a dispatch, whatever the version. The run
passes: it is a skip, not a refusal. The hold is a file in the tree, so a
`v2.x` branch cut before it was added does not carry it, and 2.x maintenance
releases go on as described above.

The pull request that cuts 3.0.0 lifts the hold: it deletes
`.github/release-hold` in the same change that sets `RELEASE_LINE = 3`. The
tests in `scripts/ci/release-line.bats` that read the real tree change with it.

## The Nix flake is not a release channel

`flake.nix` builds whatever ref it is pointed at, and nothing in `cd.yml`
touches it. A flake cannot read git tags, so it cannot know which release a
commit is. It takes the base from `Cargo.toml`, drops the `-dev`, and adds the
commit's date: `0.0.0-unstable-2026-09-22`. `thurbox-cli version` also shows
the short hash.

That string works because of two separate checks. `build.rs` turns on the
`dev_build` cfg only for a version containing `-dev`, and that cfg moves a
binary onto the `thurbox-dev` socket and data directory. A Nix install has to
share the release's sessions, so it can't carry `-dev`. The runtime
`is_dev_build()` also treats a `0.0.0` version as unreleased, and that keeps
auto-update and `thurbox-cli update` from trying to replace a binary in the
read-only Nix store. Anyone who wants a release pins the flake input to its
tag. The binary still reports `0.0.0-unstable-…`, dated to the tagged commit. Being unreleased has one
more effect: `official_ref()` fetches official extensions from `main` rather
than from a tag, which is the same thing a source build does.

## Checklist for a release that changes artifacts

- [ ] `thurbox` and `thurbox-cli` are still in **both** archive steps of `cd.yml`
      (the `tar czf` line and the `Compress-Archive` line).
- [ ] A newly added binary is in both archive steps *and* in every channel:
      `packaging/homebrew/Formula/thurbox.rb`, `packaging/aur/thurbox/PKGBUILD`,
      `packaging/aur/thurbox-bin/PKGBUILD`,
      `packaging/winget/manifests/*installer.yaml`, `scripts/install.sh`,
      `scripts/install.ps1`, and `nix/package.nix` (`cargoBuildFlags` and the
      wrapper loop).
- [ ] Installers handle its **absence**, so installing an older release still
      works (`scripts/install.bats` asserts this).
- [ ] Adding to `BINARIES` was a deliberate decision, not a reflex.
- [ ] Chocolatey is throttled to one publish per 30 days and winget attempts
      every release but skips while its previous PR is still open, so both
      channels can lag behind GitHub Releases. Do not treat a missing package
      version as a failure.
