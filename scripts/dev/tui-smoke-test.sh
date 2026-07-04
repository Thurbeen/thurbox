#!/usr/bin/env bash
# Moved: scripts/dev/tui-smoke-test.sh → scripts/dev/smoke/tui-smoke.sh
# Thin compatibility shim (kept for muscle memory / external notes).
# See scripts/dev/README.md.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
printf '\033[1;33mnote:\033[0m tui-smoke-test.sh moved to smoke/tui-smoke.sh — please update your reference.\n' >&2
exec "$here/smoke/tui-smoke.sh" "$@"
