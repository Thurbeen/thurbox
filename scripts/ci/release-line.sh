#!/usr/bin/env bash
#
# release-line.sh <version> <channels-major>
#
# The release gate for major versions, run by cd.yml's check-release before
# anything is tagged. Two decisions, both printed as `key=value` lines for
# $GITHUB_OUTPUT:
#
# 1. The version must be on the line the source declares
#    (`RELEASE_LINE` in crates/thurbox/src/update/version_check.rs). `cog bump --auto` turns
#    any `BREAKING CHANGE:` footer or `feat!:` into a new major, and a squash
#    merge carries branch commit bodies into the commit on main, so without
#    this a stray footer cuts 3.0.0 by itself. With it, a new major needs a
#    reviewed change to RELEASE_LINE, and every older line's updater knows only
#    its own. Exits 1 when the version is off the line.
#
# 2. `channels=true` when the version should go to the existing package names
#    (Homebrew `thurbox`, AUR `thurbox`/`thurbox-bin`, Chocolatey `thurbox`,
#    winget `Thurbeen.thurbox`): its major is <channels-major> and it is the
#    highest tag of that major. Those names stay on that major for good, so a
#    user who never opts in to the next one never gets it from `brew upgrade`.
#
# THURBOX_RELEASE_LINE_SOURCE overrides the source file (tests).

set -euo pipefail

version="${1:?usage: release-line.sh <version> <channels-major>}"
channels_major="${2:?usage: release-line.sh <version> <channels-major>}"
source_file="${THURBOX_RELEASE_LINE_SOURCE:-crates/thurbox/src/update/version_check.rs}"

bare="${version#v}"
major="${bare%%.*}"
if ! [[ "$major" =~ ^[0-9]+$ ]]; then
  echo "::error::cannot read a major version from '$version'" >&2
  exit 1
fi

line="$(sed -nE 's/^pub const RELEASE_LINE: u64 = ([0-9]+);.*/\1/p' "$source_file")"
if [ -z "$line" ]; then
  echo "::error::no 'pub const RELEASE_LINE: u64 = N;' in $source_file" >&2
  exit 1
fi

if [ "$major" != "$line" ]; then
  echo "::error::refusing to release $version: the source is on the $line.x line (RELEASE_LINE in $source_file). A new major is never cut by a commit message; change RELEASE_LINE in a reviewed pull request and dispatch the Release workflow with the exact version. See docs/thurbox/releasing.md, \"v2 and v3\"." >&2
  exit 1
fi

channels=false
if [ "$major" = "$channels_major" ]; then
  highest="$(git tag --list "v${major}.*" | sort -V | tail -1)"
  if [ -z "$highest" ] \
    || [ "$(printf '%s\n%s\n' "$highest" "v$bare" | sort -V | tail -1)" = "v$bare" ]; then
    channels=true
  fi
fi

echo "line=$line"
echo "channels=$channels"
