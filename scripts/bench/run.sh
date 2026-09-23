#!/usr/bin/env bash
#
# Benchmark raw tmux vs Herdr vs thurbox — the one command.
#
#   scripts/bench/run.sh                       # fetch, build, run every scenario
#   scripts/bench/run.sh --quick --reps 1      # try the harness end to end
#   scripts/bench/run.sh --scenarios latency   # one scenario (see run.py --help)
#
# Everything it creates lives under ${BENCH_CACHE:-~/.cache/thurbox-bench}:
# the pinned Herdr binary, the sandboxes, and results-<timestamp>/. Nothing is
# installed system-wide, and no server it starts outlives the run.
#
# It builds thurbox in release from THIS checkout (nice -n 10), then runs the
# timed scenarios at the shell's own niceness — say `git switch --detach
# origin/main` first to measure main. tmux is whatever is on PATH; under
# `nix develop` that is the flake's.
set -euo pipefail

HERDR_VERSION=v0.9.1
HERDR_SHA256=2a02fed16beb651ef006e1d43f048f652ca4dc58ad053cd2d44450563d5c54b7

repo=$(cd "$(dirname "$0")/../.." && pwd)
cache=${BENCH_CACHE:-$HOME/.cache/thurbox-bench}
herdr_dir=$cache/herdr-$HERDR_VERSION
export BENCH_CACHE=$cache

build=1
args=()
for arg in "$@"; do
    case "$arg" in
        --no-build) build=0 ;;
        *) args+=("$arg") ;;
    esac
done

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) asset=herdr-linux-x86_64 ;;
    Linux-aarch64) asset=herdr-linux-aarch64 ;;
    *)
        echo "run.sh: the harness reads /proc, so it runs on Linux only" >&2
        exit 2
        ;;
esac

# Herdr's documented manual install: the release binary, made executable, put
# on a path of our choosing. Pinned by version and checked by hash, because a
# benchmark of "whatever was latest" cannot be re-run.
if [ ! -x "$herdr_dir/herdr" ]; then
    mkdir -p "$herdr_dir"
    curl -fsSL -o "$herdr_dir/herdr.part" \
        "https://github.com/herdrdev/herdr/releases/download/$HERDR_VERSION/$asset"
    if [ "$asset" = herdr-linux-x86_64 ]; then
        echo "$HERDR_SHA256  $herdr_dir/herdr.part" | sha256sum -c --quiet -
    fi
    chmod +x "$herdr_dir/herdr.part"
    mv "$herdr_dir/herdr.part" "$herdr_dir/herdr"
fi

if [ "$build" = 1 ]; then
    (cd "$repo" && nice -n 10 cargo build --release --bin thurbox --bin thurbox-cli)
fi

command -v tmux >/dev/null || {
    echo "run.sh: tmux is not on PATH (nix develop provides it)" >&2
    exit 2
}

if command -v python3 >/dev/null; then
    exec python3 "$repo/scripts/bench/run.py" "${args[@]}"
fi
# The harness is standard-library Python; borrow an interpreter if there is none.
exec nix shell nixpkgs#python3 -c python3 "$repo/scripts/bench/run.py" "${args[@]}"
