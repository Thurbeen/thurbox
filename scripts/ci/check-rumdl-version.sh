#!/usr/bin/env bash
# Hold the local rumdl to the version CI lints markdown with, so `just lint`
# fails here instead of CI failing after a push.
#
# The pin's single source is the `rvben/rumdl@<sha> # vX.Y.Z` line of the CI
# markdown job: Renovate bumps the SHA and its comment together, and
# scripts/install-dev-tools.sh installs whatever `--print` reports.
#
#   check-rumdl-version.sh            exit 1 when `rumdl --version` differs
#   check-rumdl-version.sh --print    print the pinned version
set -euo pipefail

ci="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/.github/workflows/ci.yml"
pinned=$(sed -nE 's|.*uses: rvben/rumdl@[0-9a-f]+ # v([0-9]+\.[0-9]+\.[0-9]+).*|\1|p' "$ci" | head -n 1)
if [ -z "$pinned" ]; then
  echo "check-rumdl-version: no 'rvben/rumdl@<sha> # vX.Y.Z' pin in $ci" >&2
  exit 2
fi

if [ "${1:-}" = "--print" ]; then
  echo "$pinned"
  exit 0
fi

hint="install it with scripts/install-dev-tools.sh (or: cargo install --locked rumdl@$pinned)"
if ! command -v rumdl >/dev/null 2>&1; then
  echo "rumdl is not on PATH; CI lints with $pinned — $hint" >&2
  exit 1
fi
installed=$(rumdl --version | awk '{print $2}')
if [ "$installed" != "$pinned" ]; then
  echo "rumdl $installed is installed but CI lints with $pinned — $hint" >&2
  exit 1
fi
