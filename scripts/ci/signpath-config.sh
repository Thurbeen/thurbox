#!/usr/bin/env bash
# Decide whether this release signs its Windows executables through SignPath,
# and write `enabled=true|false` to $GITHUB_OUTPUT.
#
# Signing is opt-in by configuration, so a fork, and this repository until its
# SignPath Foundation project is approved, keeps releasing exactly as before:
# with nothing set the step reports `enabled=false` and exits green. Every
# setting present signs. Some but not all is a misconfiguration and fails the
# job, naming what is missing: shipping unsigned then would hide the mistake
# until a user's Defender noticed (#1576).
#
# The token arrives from a secret and the ids from repository variables; only
# their presence is read here, never printed. docs/CODE-SIGNING.md lists them.
set -euo pipefail

settings=(
  SIGNPATH_API_TOKEN
  SIGNPATH_ORGANIZATION_ID
  SIGNPATH_PROJECT_SLUG
  SIGNPATH_SIGNING_POLICY_SLUG
)

missing=()
for name in "${settings[@]}"; do
  if [ -z "${!name:-}" ]; then
    missing+=("$name")
  fi
done

if [ "${#missing[@]}" -eq "${#settings[@]}" ]; then
  echo "enabled=false" >>"$GITHUB_OUTPUT"
  echo "::notice::SignPath is not configured; the Windows executables ship unsigned. See docs/CODE-SIGNING.md."
  exit 0
fi

if [ "${#missing[@]}" -gt 0 ]; then
  echo "::error::SignPath is partly configured; missing: ${missing[*]}. Set every one, or none, as docs/CODE-SIGNING.md lists."
  exit 1
fi

echo "enabled=true" >>"$GITHUB_OUTPUT"
echo "SignPath is configured; signing the Windows executables."
