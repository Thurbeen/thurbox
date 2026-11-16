#!/usr/bin/env bash
#
# Install a release from scratch and check it runs: the documented one-line
# installer (install.sh, or install.ps1 on Windows) against a local copy of the
# release assets, into an empty HOME, with nothing thurbox or Fleet already on
# PATH. Then every shipped program must resolve to the install directory and
# answer. `fleet init` must write a config, and `thurbox-cli doctor` must find
# Fleet, its config and runtime-mux.
#
# With --fleet-only it runs the installers' fleet-only mode instead: only
# Fleet's programs may be installed, neither thurbox nor thurbox-cli, and
# `fleet doctor` takes thurbox-cli doctor's place.
#
# Usage:
#   scripts/ci/install-e2e.sh <dist-dir> <version> [--fleet-only]
#
# <dist-dir> holds the archive scripts/ci/release-archive.sh packed for this
# machine; this script serves it with the sums file release-archive.sh --sums
# writes beside it, as the Release workflow publishes them. Needs python3 (to
# serve the assets) and jq.
#
# The multiplexer is the machine's business, not the installer's: when none is
# on PATH, doctor's `multiplexer` check is the one failure allowed.

set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
case "$#:${3:-}" in
  2: | 3:--fleet-only) ;;
  *) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
dist="$(cd "$1" && pwd)" version="$2" fleet_only="${3:-}"
# shellcheck disable=SC2207 # one word per line; macOS's bash 3.2 has no mapfile
shipped=($("$repo/scripts/ci/release-archive.sh" --binaries))
# shellcheck disable=SC2207
binaries=($("$repo/scripts/ci/release-archive.sh" "--${fleet_only:+fleet-}binaries"))

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) windows=1 ;;
  *) windows="" ;;
esac
python=python3
command -v "$python" >/dev/null || python=python

root="$(mktemp -d)"
server=""
cleanup() {
  [ -n "$server" ] && kill "$server" 2>/dev/null
  rm -rf "$root"
}
trap cleanup EXIT

# Laid out as the release download URL is: <base>/<version>/<asset>.
www="$root/www/$version"
mkdir -p "$www"
cp "$dist"/thurbox-* "$www/" 2>/dev/null || fail "no archive in $dist"
"$repo/scripts/ci/release-archive.sh" --sums "$version" "$www" >/dev/null

port=$(( 20000 + RANDOM % 20000 ))
"$python" -m http.server "$port" --bind 127.0.0.1 --directory "$root/www" >/dev/null 2>&1 &
server=$!
for _ in $(seq 1 50); do
  curl -fsS -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null && break
  sleep 0.2
done

# A machine that has never seen thurbox: an empty home, and a PATH without
# any directory that already holds one of the shipped programs, whichever
# the install takes.
bin="$root/bin"
export HOME="$root/home"
mkdir -p "$HOME"
unset XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME FLEET_CONFIG \
  THURBOX_CONFIG_DIR THURBOX_DATA_DIR THURBOX_SOCKET THURBOX_SOCKET_FOR THURBOX_SESSION \
  TMUX TMUX_TMPDIR
if [ -n "$windows" ]; then
  export USERPROFILE APPDATA LOCALAPPDATA
  USERPROFILE="$(cygpath -w "$HOME")"
  APPDATA="$(cygpath -w "$HOME/AppData/Roaming")"
  LOCALAPPDATA="$(cygpath -w "$HOME/AppData/Local")"
  mkdir -p "$HOME/AppData/Roaming" "$HOME/AppData/Local"
fi
clean_path=""
IFS=: read -r -a entries <<< "$PATH"
for dir in "${entries[@]}"; do
  for b in "${shipped[@]}"; do
    if [ -e "$dir/$b" ] || [ -e "$dir/$b.exe" ]; then continue 2; fi
  done
  clean_path="${clean_path:+$clean_path:}$dir"
done
export PATH="$clean_path"
for b in "${shipped[@]}"; do
  if command -v "$b" >/dev/null; then fail "$b is already on PATH"; fi
done

url="http://127.0.0.1:$port"
if [ -n "$windows" ]; then
  THURBOX_VERSION="$version" THURBOX_DOWNLOAD_URL="$url" \
    THURBOX_INSTALL_DIR="$(cygpath -w "$bin")" \
    pwsh -NoProfile -File "$(cygpath -w "$repo/scripts/install.ps1")" ${fleet_only:+-FleetOnly} \
    || fail "install.ps1 exited $?"
else
  VERSION="$version" DOWNLOAD_URL="$url" INSTALL_DIR="$bin" \
    sh "$repo/scripts/install.sh" ${fleet_only:+"$fleet_only"} || fail "install.sh exited $?"
fi
pass "the installer ran"

export PATH="$bin:$PATH"
for b in "${binaries[@]}"; do
  found="$(command -v "$b" || true)"
  [ -n "$found" ] || fail "$b was not installed"
  [ "$(cd "$(dirname "$found")" && pwd)" = "$(cd "$bin" && pwd)" ] \
    || fail "$b resolves to $found, not the install directory"
done
pass "installed: ${binaries[*]}"

if [ -n "$fleet_only" ]; then
  for b in thurbox thurbox-cli; do
    if [ -e "$bin/$b" ] || [ -e "$bin/$b.exe" ]; then fail "a fleet-only install wrote $b"; fi
    if command -v "$b" >/dev/null; then fail "$b is on PATH"; fi
  done
  pass "no thurbox or thurbox-cli installed or on PATH"
else
  thurbox --version </dev/null || fail "thurbox --version exited $?"
  thurbox-cli version || fail "thurbox-cli version exited $?"
  pass "thurbox --version and thurbox-cli version answer"
fi
fleet version || fail "fleet version exited $?"
pass "fleet version answers"

# The two programs Fleet and runtime-mux run speak JSON on stdin: an unknown
# request comes back as an `unknown` answer, which only a program that runs
# here can give.
for b in runtime-mux workspace-git; do
  answer="$("$b" x </dev/null)" || fail "$b exited $?"
  echo "$answer" | jq -e '.unknown' >/dev/null || fail "$b answered: $answer"
done
pass "runtime-mux and workspace-git answer"

fleet init || fail "fleet init exited $?"
pass "fleet init wrote a config"

if [ -n "$fleet_only" ]; then
  # Fleet checks this machine on its own: harnesses, integrations and the
  # runtime of each place. runtime-mux answers only beside a multiplexer.
  status=0
  report="$(fleet doctor --json)" || status=$?
  echo "$report" | jq . >/dev/null 2>&1 || fail "fleet doctor --json printed no JSON: $report"
  fleet doctor || true
  failing="$(echo "$report" | jq -r '[.checks[] | select(.level == "fail") | .area + ":" + .name] | join(" ")')"
  mux=tmux
  [ -n "$windows" ] && mux=psmux
  if command -v "$mux" >/dev/null; then
    [ -z "$failing" ] && [ "$status" -eq 0 ] \
      || fail "fleet doctor exited $status with failing checks: $failing"
  else
    [ "$failing" = "runtime:mux/local" ] || fail "fleet doctor's failing checks: $failing"
    echo "note: $mux is not on PATH; fleet doctor's mux/local runtime check fails, as it should"
  fi
  pass "fleet doctor finds the mux/local runtime"
  exit 0
fi

status=0
report="$(thurbox-cli doctor --json)" || status=$?
echo "$report" | jq . >/dev/null 2>&1 || fail "doctor --json printed no JSON: $report"
thurbox-cli doctor || true
for key in fleet fleet-config runtime-mux; do
  level="$(echo "$report" | jq -r --arg k "$key" '[.checks[] | select(.key == $k) | .level][0]')"
  [ "$level" = "ok" ] || fail "doctor's $key check is $level, not ok"
done
failing="$(echo "$report" | jq -r '[.checks[] | select(.level == "fail") | .key] | join(" ")')"
mux="$(echo "$report" | jq -r .multiplexer)"
if command -v "$mux" >/dev/null; then
  [ -z "$failing" ] && [ "$status" -eq 0 ] \
    || fail "doctor exited $status with failing checks: $failing"
else
  [ "$failing" = "multiplexer" ] || fail "doctor's failing checks: $failing"
  echo "note: $mux is not on PATH; doctor's multiplexer check fails, as it should"
fi
pass "doctor finds Fleet, its config and runtime-mux"
