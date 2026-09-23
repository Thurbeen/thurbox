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

# Like scripts/dev/perf-run.sh, and for the same reason: this builds a release
# binary and times things for over an hour, so an agent deciding what "run the
# tests" means inside a validation step must not reach for it. run.py refuses
# too, for the scenario scripts run on their own; this one refuses before the
# download and the build.
if [ -n "${THURBOX_GATE:-}" ] && [ -z "${THURBOX_PERF_ALLOW_IN_GATE:-}" ]; then
    echo "run.sh: refusing to run inside a validation step (THURBOX_GATE is set)." >&2
    echo "This is a benchmark, not a test; run it by hand on a quiet machine." >&2
    exit 2
fi

HERDR_VERSION=v0.9.1

repo=$(cd "$(dirname "$0")/../.." && pwd)
cache=${BENCH_CACHE:-$HOME/.cache/thurbox-bench}
herdr_dir=$cache/herdr-$HERDR_VERSION-$(uname -m)
export BENCH_CACHE=$cache

build=1
hosts=tmux,herdr,thurbox
args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=0 ;;
        --hosts=*)
            hosts=${1#--hosts=}
            args+=("$1")
            ;;
        --hosts)
            hosts=${2:-}
            args+=("$1" "${2:-}")
            shift
            ;;
        *) args+=("$1") ;;
    esac
    shift
done

# The digests GitHub records for the release assets.
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        asset=herdr-linux-x86_64
        sha256=2a02fed16beb651ef006e1d43f048f652ca4dc58ad053cd2d44450563d5c54b7
        ;;
    Linux-aarch64)
        asset=herdr-linux-aarch64
        sha256=f4ccf4de745f2cb9a39a983e9ba3703dad50ec2a58dea83026ceab721bbd8d9e
        ;;
    *)
        echo "run.sh: the harness reads /proc, so it runs on Linux only" >&2
        exit 2
        ;;
esac

# Herdr's documented manual install: the release binary, made executable, put
# on a path of our choosing. Pinned by version and checked by hash — on every
# run, not only on download, because a benchmark of "whatever binary was lying
# in the cache" cannot be re-run either.
herdr_args=()
case ",$hosts," in
    *,herdr,*)
        if [ ! -x "$herdr_dir/herdr" ]; then
            mkdir -p "$herdr_dir"
            curl -fsSL -o "$herdr_dir/herdr.part" \
                "https://github.com/herdrdev/herdr/releases/download/$HERDR_VERSION/$asset"
            chmod +x "$herdr_dir/herdr.part"
            mv "$herdr_dir/herdr.part" "$herdr_dir/herdr"
        fi
        echo "$sha256  $herdr_dir/herdr" | sha256sum -c --quiet -
        herdr_args=(--herdr "$herdr_dir/herdr")
        ;;
esac

if [ "$build" = 1 ] && [[ ",$hosts," == *,thurbox,* ]]; then
    (cd "$repo" && nice -n 10 cargo build --release --bin thurbox --bin thurbox-cli)
fi

command -v tmux >/dev/null || {
    echo "run.sh: tmux is not on PATH (nix develop provides it)" >&2
    exit 2
}

if command -v python3 >/dev/null; then
    exec python3 "$repo/scripts/bench/run.py" "${herdr_args[@]}" "${args[@]}"
fi
# The harness is standard-library Python; borrow an interpreter if there is none.
exec nix shell nixpkgs#python3 -c python3 "$repo/scripts/bench/run.py" \
    "${herdr_args[@]}" "${args[@]}"
