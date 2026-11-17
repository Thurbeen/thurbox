# Releasing

Releases are automated (`cd.yml`, driven by cocogitto) and the mechanics are in
the `thurbox-release` skill. This file is for the part automation cannot check: **what a release
may and may not change about the artifacts**, because most users never read
release notes — `[features] auto_update` defaults to `true`, so the TUI replaces
its own binaries on startup.

## The rule that matters

> **Never remove a program the updater replaces from a release archive.**

The updater in the *already installed* binary decides what to install, and it
hard-fails on a known binary that is missing from the tarball:

    // src/update/self_update.rs
    const BINARIES: [&str; 5] =
        ["thurbox", "thurbox-cli", "fleet", "runtime-mux", "workspace-git"];
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

Every 2.x updater names `thurbox` and `thurbox-cli`; one built from this tree
names all five programs a v3 archive ships. Renaming what a binary *does* is
fine. Renaming or dropping the file is not.

## Adding a binary

What an added binary does to existing installs:

1. The updater only looks for the names in its own `BINARIES`, so an unknown
   extra file in the archive is ignored by every updater already shipped.
2. 2.x updaters skip a known name that is not already on disk, so they never
   introduce a binary. An updater built from this tree installs every name in
   its `BINARIES`, present or not: a v3 install that lacks `fleet`,
   `runtime-mux` or `workspace-git` cannot start a session, and the archive is
   the same verified one.
3. Every distribution channel names binaries explicitly — Homebrew
   (`bin.install`), both AUR PKGBUILDs, winget (`NestedInstallerFiles`), and both
   installers. Only Chocolatey globs, because it auto-shims every `.exe`.

So a binary added to `BINARIES` reaches existing v3 installs on their next
update. One added only to the archive reaches nobody until they reinstall.

## Maintenance releases

An older line is released from its own branch (`v1.x`, `v2.x`) by dispatching
`cd.yml` from that ref with an explicit `version` — `cog bump --auto` computes
from tags and would move the newest line instead. The dispatch runs that
branch's own `cd.yml`, not `main`'s. `cog.toml`'s
`branch_whitelist` carries `v*.x` beside `main`, without which `cog bump`
refuses on the branch. The tag is pushed by refspec: a push action that
defaults to the repository's default branch would push the maintenance tip onto
`main`, be rejected, and the tag would never leave the runner.

`cd.yml` makes two separate checks. `newest` compares the version against every
existing tag: when it is behind the highest, the GitHub Release is published
with `make_latest: false`. `channels` (`scripts/ci/release-line.sh`) decides the
package jobs: Homebrew, AUR, Chocolatey and winget get only the newest release
of `PACKAGE_CHANNELS_MAJOR`, so the newest 2.x still updates them after a 3.x
exists, and an older maintenance cut skips them. The tag, the five-target build
and the release assets are the same either way.

## v2 and v3

3.x (Fleet as thurbox's engine, one repository) is a different program under
the same binary names. `main` releases 3.x (`RELEASE_LINE = 3`); 2.x is
maintained on `v2.x`. **No 2.x install moves to 3.x unless its user reinstalls
on purpose.** Everything below exists to keep that true for installs whose
update code can no longer change. [Migrating to v3](../migrating-to-v3.md)
owns the steps an operator takes to move.

### What every shipped updater does

Every updater from v1.0.0 to v2.83.6 resolves GitHub's `releases/latest`; the
line-rule ones read the release list. Every 2.x updater then downloads
`releases/download/v<V>/thurbox-v<V>-checksums.txt` with `curl -fsSL` (or
`wget`) **before** the archive, and returns an error — replacing nothing — when
that download fails. They differ only in what they do with a newer major:

| Installed | On a 3.x "Latest" |
|---|---|
| v1.0.0 – v1.8.7, v2.0.0 – v2.2.2 | no major guard: would install 3.x |
| v2.2.3 – v2.83.6 | `crosses_major`: report 3.x, install nothing, and never see a later 2.x either |
| v2.89.0 onwards | the line rule below: install the newest 2.x, report 3.x |

### The line rule (v2.89.0 onwards)

`update::version_check::RELEASE_LINE` is the major this source tree releases.
The update check, the startup auto-update and `thurbox-cli update` read GitHub's
release **list** and only consider releases on that line, so a 2.x maintenance
release cut after 3.0.0 — published with `make_latest: false` — is still found
and installed. A newer major is reported (`thurbox-cli version --check`,
`thurbox-cli update`, a log line on startup) with a link here, and never
installed: `--force` reinstalls the newest release of the line, nothing else.
The header badge only offers the line. 3.x carries the same rule with
`RELEASE_LINE = 3`.

### What a 3.x release must do

1. **Never publish an asset named `thurbox-v<V>-checksums.txt`.** That one name
   is what every pre-line-rule updater downloads first, so its absence makes all
   of them fail safe — logged as `auto-update failed`, binaries untouched —
   whatever `releases/latest` points at. This is the only protection the v1.x
   and v2.0.0–v2.2.2 installs have, and it is a property of the 3.x release, not
   of any code they carry. A 3.x release's sums file is
   `thurbox-<V>-sha256sums.txt` (no `v`): `scripts/ci/release-archive.sh --sums`
   writes it, `cd.yml` publishes it, and the 3.x updater, both installers and
   CI's `install-e2e` job read it. The installers fall back to
   `thurbox-v<V>-checksums.txt` only for a pinned 2.x release.
   `scripts/install.bats` fails when `cd.yml` names the old file.
2. **"Latest"**: 3.0.0 is GitHub's "Latest". Fresh one-liner installs get 3.x,
   and v2.2.3–v2.83.6 installs stop auto-updating (they see 3.x, skip it, and
   never look for a 2.x) until their user runs a 2.x installer once. That is safe
   but stale. A 2.x maintenance release cut after 3.0.0 is behind the highest
   tag, so `cd.yml`'s `newest` step publishes it with `make_latest: false`, and
   only the line-rule updaters find it.
3. **Package channels**: the existing names — Homebrew `thurbox`, AUR `thurbox`
   and `thurbox-bin`, Chocolatey `thurbox`, winget `Thurbeen.thurbox` — carry 2.x
   for good, so `brew upgrade`, an AUR helper, `choco upgrade` or `winget upgrade`
   never moves anyone across. 3.x has no package channel yet: it ships through
   the GitHub Release, `scripts/install.{sh,ps1}` and the Nix flake. Names of
   its own are chosen when it gets one. `cd.yml` enforces this:
   `PACKAGE_CHANNELS_MAJOR` is `2`, so a 3.x release skips every channel job
   (`channels=false`), and only the newest 2.x tag is pushed to those names
   (`scripts/ci/release-line.sh`), including a 2.x maintenance release cut after
   3.x exists.
4. **Installers**: `scripts/install.{sh,ps1}` install `releases/latest` unless
   given a version, so a 2.x user re-running the one-liner gets 3.x.
   `VERSION=v2.x.y` (`THURBOX_VERSION` on Windows) pins 2.x. A Nix flake input
   that follows `main` builds 3.x on its next `nix flake update`; point it at
   `v2.x` or a 2.x tag to stay on 2.x.
5. **State**: 3.x keeps 2.x's names: the config directory `~/.config/thurbox`,
   the data directory `~/.local/share/thurbox`, the interface's `thurbox` tmux
   socket, and Fleet's `~/.config/fleet` and `~/.local/share/fleet`. Fleet 3's
   `runtime-mux` runs Task sessions on a socket of its own, `runtime-mux`. So
   3.x replaces 2.x on a machine. The two cannot run side by side: they
   install the same programs, share those directories, and the agent hooks
   call whichever `thurbox-cli` is on `PATH`. What 3.x does with what 2.x left:
   - `settings.toml` loads. A key 3.x no longer reads is reported, not refused.
   - `hosts.toml` and `automations.json` are not read. `thurbox-cli doctor`
     warns about each and names what replaces it.
   - `thurbox.db` is not read or written: 3.x has no database, and Fleet holds
     its sessions. 2.x sessions do not carry over.
   - A Fleet 2.x `config.toml` is not a Fleet 3 config. Every `fleet` command,
     `fleet up` included, refuses it by name and says what to do.
     Fleet 3 cannot read the objects Fleet 2.x wrote to its store either, and
     `fleet init` points Fleet 3 at the same store directory, so both move
     aside together.

   What a user does is in [Migrating to v3](../migrating-to-v3.md). To
   stay on 2.x instead, install with `VERSION=v2.x.y`: its updater then stays
   on 2.x.

### Cutting the major, and 2.x after it

`cog bump --auto` turns a `BREAKING CHANGE:` footer or a `feat!:` into a new
major, and a squash merge can carry a branch commit's body onto `main`. So
`cd.yml` runs `scripts/ci/release-line.sh` before tagging and **refuses any
version whose major is not `RELEASE_LINE`** — on a push or a dispatch alike. If a
breaking footer slips onto `main`, the Release run fails instead of tagging
the next major; cut the release it owed by dispatching with an explicit
`version`.

3.0.0 therefore takes one deliberate step: a reviewed pull request that sets
`RELEASE_LINE = 3` and does what the list above asks. `main` carries `feat!:`
commits since v2.88.0, so the push that merges it computes v3.0.0 with
`cog bump --auto`, and the Release run cuts it with no dispatch. Merge that pull
request only after v2.89.0 is published from `v2.x`. Tags v2.84.0–v2.88.0 sit
on `main` but their releases were never published, so v2.83.6 is the newest
2.x anyone installed, `v2.x` branches from it, and v2.89.0 is the first
published 2.x with the line rule; it must be out before 3.0.0 is "Latest". `v2.x` is
branched from the last 2.x commit; `cog.toml` already allows `v*.x`. A 2.x
maintenance release is cut by dispatching `cd.yml` from `v2.x` with an explicit
version: it is tagged and built, published without "Latest" while a 3.x tag is
higher, and still pushed to the 2.x package names as the newest 2.x.

### The v3 hold

While `.github/release-hold` exists, `scripts/ci/release-line.sh` answers
`held=true` and `cd.yml` tags and publishes nothing, on a push or a dispatch,
whatever the version. The run passes: it is a skip, not a refusal. `main` held
its releases this way while it carried unreleased 3.x work on line 2, and the
pull request that set `RELEASE_LINE = 3` deleted the file. With
`RELEASE_LINE = 3`, a 2.x version from `main` is refused outright, so a `feat`
or `fix` on `main` can no longer cut a 2.x. `scripts/ci/release-line.bats`
checks the real tree: not held, 3.0.0 passes to no channel, a 2.x is refused.
The mechanism stays for the next major's unreleased work.

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
more effect: `official_ref()` fetches official panes from `main` rather
than from a tag, which is the same thing a source build does.

## Checklist for a release that changes artifacts

- [ ] The archive ships what `BINARIES` in `scripts/ci/release-archive.sh`
      lists: `thurbox`, `thurbox-cli`, `fleet`, `runtime-mux` and
      `workspace-git`. `cd.yml` builds and packs that list, and CI's
      `install-e2e` job installs the packed archive on a clean Linux, macOS and
      Windows machine.
- [ ] A newly added binary is in that list, in both installers'
      `BINARIES` (`scripts/install.sh`, `scripts/install.ps1`; `install.bats`
      and `install.Tests.ps1` fail when they differ), and in every channel:
      `packaging/homebrew/Formula/thurbox.rb`, `packaging/aur/thurbox/PKGBUILD`,
      `packaging/aur/thurbox-bin/PKGBUILD`,
      `packaging/winget/manifests/*installer.yaml`, and `nix/package.nix`
      (`cargoBuildFlags` and the wrapper loop).
- [ ] Installers handle its **absence**, so installing an older release still
      works (`scripts/install.bats` asserts this).
- [ ] Adding to `BINARIES` was a deliberate decision, not a reflex.
- [ ] Chocolatey is throttled to one publish per 30 days and winget attempts
      every release but skips while its previous PR is still open, so both
      channels can lag behind GitHub Releases. Do not treat a missing package
      version as a failure.
