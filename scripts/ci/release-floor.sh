#!/usr/bin/env bash

set -euo pipefail

version="${1:?usage: release-floor.sh <version>}"
bare="${version#v}"
major="${bare%%.*}"
if ! [[ "$major" =~ ^[0-9]+$ ]]; then
  echo "::error::cannot read a major version from '$version'" >&2
  exit 1
fi

highest="$(git tag --list "v${major}.*" | sort -V | tail -1)"
if [ -z "$highest" ] \
  || { [ "v$bare" != "$highest" ] \
    && [ "$(printf '%s\n%s\n' "$highest" "v$bare" | sort -V | tail -1)" = "v$bare" ]; }; then
  echo "v$bare"
  exit 0
fi

rest="${highest#v"${major}".}"
minor="${rest%%.*}"
echo "v${major}.$((minor + 1)).0"
