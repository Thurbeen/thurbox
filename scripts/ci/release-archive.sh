#!/usr/bin/env bash
set -euo pipefail

BINARIES=(thurbox thurbox-cli fleet runtime-mux runtime-herdr workspace-git)
FLEET_BINARIES=(fleet runtime-mux runtime-herdr workspace-git)

usage() {
  cat <<'EOF'

Pack one target's release archive: every program a v3 install runs by name,
plus LICENSE. The Release workflow (cd.yml) and the clean-install test in CI
both pack through here, so the test installs what a release ships.

Usage:
  scripts/ci/release-archive.sh <version> <target> <bin-dir> [out-dir]
  scripts/ci/release-archive.sh --binaries     # print the shipped programs
  scripts/ci/release-archive.sh --fleet-binaries  # print the ones Fleet needs
  scripts/ci/release-archive.sh --sums <version> <dir>  # write the sums file

Writes thurbox-<version>-<target>.tar.gz, or .zip for a Windows target (the
archive install.ps1 and the in-app updater expect), into out-dir (default:
the working directory) and prints its path.

--sums writes the SHA-256 of every archive in <dir> to
thurbox-<version without v>-sha256sums.txt there and prints its path. Never
thurbox-v<version>-checksums.txt: every 2.x updater fetches that name first,
and its absence is what makes them fail safe on a 3.x release
(docs/thurbox/releasing.md, "What a 3.x release must do").
EOF
}

case "${1:-}" in
  --binaries) printf '%s\n' "${BINARIES[@]}"; exit 0 ;;
  --fleet-binaries) printf '%s\n' "${FLEET_BINARIES[@]}"; exit 0 ;;
  --sums)
    [ "$#" -eq 3 ] || { usage >&2; exit 2; }
    version="${2#v}" dir="$(cd "$3" && pwd)"
    sums="thurbox-${version}-sha256sums.txt"
    sum=sha256sum
    command -v "$sum" >/dev/null || sum="shasum -a 256"
    (
      cd "$dir"
      shopt -s nullglob
      archives=(thurbox-*.tar.gz thurbox-*.zip)
      [ "${#archives[@]}" -gt 0 ] || { echo "release-archive: no archive in $dir" >&2; exit 1; }
      $sum "${archives[@]}" > "$sums"
    )
    echo "$dir/$sums"
    exit 0
    ;;
esac

if [ "$#" -lt 3 ]; then
  usage >&2
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
  # install.ps1's Expand-Archive and the updater read Compress-Archive's zip, not Git Bash's.
  (cd "$stage" && pwsh -NoProfile -Command \
    "Compress-Archive -Path * -DestinationPath '$(cygpath -w "$archive" 2>/dev/null || echo "$archive")'")
else
  archive="$out_dir/$base.tar.gz"
  tar -czf "$archive" -C "$stage" "${BINARIES[@]}" LICENSE
fi
echo "$archive"
