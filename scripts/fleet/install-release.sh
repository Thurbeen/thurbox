#!/bin/sh
set -eu

repo=Thurbeen/fleet
base=${FLEET_RELEASE_BASE:-https://github.com/$repo/releases/download}
dir=${FLEET_INSTALL_DIR:-$HOME/.local/bin}

fail() {
    printf 'install-release: %s\n' "$*" >&2
    exit 1
}

system=$(uname -s)
machine=$(uname -m)
unsupported="unsupported platform: $system $machine. The release builds x86_64-unknown-linux-musl, aarch64-unknown-linux-musl and aarch64-apple-darwin"
case "$system/$machine" in
    Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
    Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    *) fail "$unsupported" ;;
esac

mkdir -p "$dir"
lock="$dir/.fleet-install.lock"
mkdir "$lock" 2>/dev/null || fail "install lock exists at $lock; another installer may be running"
tmp=
staged=
cleanup() {
    [ -z "$tmp" ] || rm -rf "$tmp"
    [ -z "$staged" ] || rm -f "$staged"
    rmdir "$lock"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
case "${1:-}" in
    --uninstall)
        [ "$#" = 1 ] || fail "usage: install-release.sh [--uninstall]"
        rm -f "$dir/fleet"
        printf 'uninstalled fleet from %s\n' "$dir"
        exit 0 ;;
    '') [ "$#" = 0 ] || fail "unexpected empty argument" ;;
    *) fail "usage: install-release.sh [--uninstall]" ;;
esac

release_version=
version=${FLEET_VERSION:-$release_version}
[ -n "$version" ] || fail "Name the release explicitly, for example FLEET_VERSION=v2.0.0; see the README for frozen v1 installs."

printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$' || fail "invalid release version: $version"
archive="fleet-$version-$target.tar.gz"
sums="fleet-$version-checksums.txt"

tmp=$(mktemp -d "$dir/.fleet-download.XXXXXX")

fetch() {
    asset=$1
    case "$base" in
        https://*) curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$asset" "$base/$version/$asset" ;;
        *://*) fail "FLEET_RELEASE_BASE must be an https URL or a directory: $base" ;;
        *) cp "$base/$version/$asset" "$tmp/$asset" ;;
    esac || fail "could not fetch $asset for $version from $base"
}

fetch "$archive"
fetch "$sums"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
else
    fail "neither sha256sum nor shasum is installed, so the download cannot be verified"
fi
expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$tmp/$sums")
[ -n "$expected" ] || fail "$sums lists no checksum for $archive"
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive: expected $expected, got $actual"

mkdir "$tmp/x"
tar -xzf "$tmp/$archive" -C "$tmp/x" fleet
staged="$dir/.fleet.new.$$"
cp "$tmp/x/fleet" "$staged"
chmod 755 "$staged"
reported=$("$staged" --version) || fail "candidate binary cannot report its version"
[ "$reported" = "fleet ${version#v}" ] || fail "candidate version mismatch: expected fleet ${version#v}"
mv -f "$staged" "$dir/fleet"
printf 'installed fleet %s (%s) to %s/fleet\n' "$version" "$target" "$dir"
# shellcheck disable=SC2016
case ":${PATH:-}:" in
    *":$dir:"*) ;;
    *) printf 'Add the install directory to PATH: export PATH="%s:$PATH"\n' "$dir" ;;
esac
