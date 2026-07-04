#!/usr/bin/env bash
# Moved: scripts/dev/lab-test.sh → scripts/dev/e2e/real-host.sh
# Thin compatibility shim (kept for muscle memory / external notes).
# See scripts/dev/README.md.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
printf '\033[1;33mnote:\033[0m lab-test.sh moved to e2e/real-host.sh — please update your reference.\n' >&2
exec "$here/e2e/real-host.sh" "$@"
