#!/usr/bin/env bash
set -euo pipefail

REPO="${THURBOX_MEDIA_REPO:-Thurbeen/thurbox}"
TAG=media
cd "$(git rev-parse --show-toplevel)"

if (($# == 0)); then
  shopt -s nullglob
  set -- media/*.gif media/*.mp4
fi
if (($# == 0)); then
  echo "no recordings in media/ - run scripts/demo/record*.sh first" >&2
  exit 1
fi

if ! gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  gh release create "$TAG" --repo "$REPO" --prerelease --latest=false \
    --title "Demo media" \
    --notes "Demo recordings for the README, docs and website, replaced in place by scripts/demo/publish-media.sh. Not a release of thurbox."
fi

gh release upload "$TAG" --repo "$REPO" --clobber "$@"
gh workflow run pages.yml --repo "$REPO"
echo "uploaded $# file(s) to $REPO release '$TAG'; website redeploy dispatched"
