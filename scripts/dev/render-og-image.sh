#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
svg="$repo_root/website/assets/og-image.svg"
png="$repo_root/website/assets/og-image.png"

find_chromium() {
  if [ -n "${CHROMIUM:-}" ] && [ -x "${CHROMIUM}" ]; then
    echo "$CHROMIUM"
    return 0
  fi
  for c in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$c" >/dev/null 2>&1; then
      command -v "$c"
      return 0
    fi
  done
  local base="${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers}"
  local found
  found="$(find "$base" -maxdepth 3 -type f \
    \( -name headless_shell -o -name chrome \) 2>/dev/null | head -n1 || true)"
  if [ -n "$found" ]; then
    echo "$found"
    return 0
  fi
  return 1
}

chromium="$(find_chromium)" || {
  echo "error: no chromium found; set CHROMIUM=/path/to/chrome" >&2
  exit 1
}

echo "rendering $svg -> $png using $chromium"
"$chromium" \
  --headless \
  --no-sandbox \
  --hide-scrollbars \
  --force-device-scale-factor=1 \
  --default-background-color=00000000 \
  --window-size=1200,630 \
  --screenshot="$png" \
  "file://$svg"

echo "wrote $png"
