#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-$ROOT/media/session-groups.gif}"
CAST="$(mktemp)"
trap 'rm -f "$CAST"' EXIT

THURBOX_DEMO_CAST="$CAST" cargo test --test frames record_session_groups_demo -- --ignored
agg --font-dir "${FONT_DIR:-/usr/share/fonts}" \
    --font-family "${FONT_FAMILY:-JetBrains Mono,DejaVu Sans Mono}" \
    --font-size 16 --theme asciinema "$CAST" "$OUT"
