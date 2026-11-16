#!/usr/bin/env bash
#
# release-floor.sh <version>
#
# Prints the version cd.yml should cut for an automatic release: <version>
# itself, or the next minor above the highest tag of its major when <version>
# is not above it.
#
# `cog bump --auto` bumps from the newest tag reachable from the branch. On a
# maintenance branch that can be behind a tag of the same major made elsewhere:
# `v2.x` branches from v2.83.6, while `main` tagged v2.84.0..v2.88.0 without
# publishing them. Cutting v2.83.7 there would publish a release that is
# neither "Latest" nor sent to the package channels, both of which follow the
# highest tag. A forced dispatch version is never passed through here.

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
