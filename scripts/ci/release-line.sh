#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: release-line.sh <version> <channels-major>}"
channels_major="${2:?usage: release-line.sh <version> <channels-major>}"
source_file="${THURBOX_RELEASE_LINE_SOURCE:-crates/thurbox/src/update/version_check.rs}"
hold_file="${THURBOX_RELEASE_HOLD_FILE:-.github/release-hold}"

if [ -f "$hold_file" ]; then
  echo "::notice::not releasing $version: releases are held ($hold_file): $(head -1 "$hold_file")" >&2
  echo "held=true"
  echo "channels=false"
  exit 0
fi

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

echo "held=false"
echo "line=$line"
echo "channels=$channels"
