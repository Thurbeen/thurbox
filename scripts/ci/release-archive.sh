#!/usr/bin/env bash
#
# Pack one target's release archive: every program a v3 install runs by name,
# plus LICENSE. The Release workflow (cd.yml) and the clean-install test in CI
# both pack through here, so the test installs what a release ships.
#
# Usage:
#   scripts/ci/release-archive.sh <version> <target> <bin-dir> [out-dir]
#   scripts/ci/release-archive.sh --binaries     # print the shipped programs
#
# Writes thurbox-<version>-<target>.tar.gz, or .zip for a Windows target (the
# archive install.ps1 and the in-app updater expect), into out-dir (default:
# the working directory) and prints its path.

set -euo pipefail

# thurbox runs `fleet`, Fleet runs `runtime-mux`, and runtime-mux runs
# `workspace-git`, each found on PATH or beside its caller. A release that
# leaves one out installs a thurbox that cannot start a session.
BINARIES=(thurbox thurbox-cli fleet runtime-mux workspace-git)

if [ "${1:-}" = "--binaries" ]; then
  printf '%s\n' "${BINARIES[@]}"
  exit 0
fi

if [ "$#" -lt 3 ]; then
  sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi

version="$1" target="$2" bin_dir="$3" out_dir="${4:-$PWD}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
license="$(cd "$(dirname "$0")/../.." && pwd)/LICENSE"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

case "$target" in
  *windows*) ext=".exe" ;;
  *) ext="" ;;
esac

for bin in "${BINARIES[@]}"; do
  src="$bin_dir/$bin$ext"
  [ -f "$src" ] || { echo "release-archive: $src is missing" >&2; exit 1; }
  cp "$src" "$stage/"
done
cp "$license" "$stage/"

base="thurbox-${version}-${target}"
if [ -n "$ext" ]; then
  archive="$out_dir/$base.zip"
  rm -f "$archive"
  # Compress-Archive, not a zip from Git Bash: its zip is the one install.ps1's
  # Expand-Archive and the updater have always read.
  (cd "$stage" && pwsh -NoProfile -Command \
    "Compress-Archive -Path * -DestinationPath '$(cygpath -w "$archive" 2>/dev/null || echo "$archive")'")
else
  archive="$out_dir/$base.tar.gz"
  tar -czf "$archive" -C "$stage" "${BINARIES[@]}" LICENSE
fi
echo "$archive"
