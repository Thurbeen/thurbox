#!/usr/bin/env bash
# parse-questions.sh — extract the last pending ===QUESTIONS=== JSON block from
# a worker's captured terminal output (the clarifying questions it is waiting on
# before it will plan or code).
#
# Reads captured output from stdin (or --file <path>).
# Exit codes:
#   0 = pending questions found, JSON printed on stdout
#   1 = nothing waiting (no block, or it was already answered/finished)
#   2 = sentinel present but JSON malformed
#
# A ===QUESTIONS=== block is "pending" only while the worker is still parked on
# it. If a ===RESULT=== marker appears AFTER the questions the worker has since
# finished, so the questions are no longer waiting (exit 1). The "worker resumed
# planning after answers" case is handled by the caller capturing only the tail
# (--lines N) plus its own memory of what it already relayed — see FLOW.md.
#
# Required JSON field: questions (a non-empty array).

set -euo pipefail

INPUT=""
if [[ "${1:-}" == "--file" ]]; then
  [[ -f "${2:-}" ]] || { echo "File not found: ${2:-}" >&2; exit 2; }
  INPUT="$(cat "$2")"
else
  INPUT="$(cat)"
fi

# Line number of the LAST "===QUESTIONS===" marker.
LAST_Q=$(printf '%s\n' "$INPUT" | grep -n '^===QUESTIONS===$' | tail -1 | cut -d: -f1 || true)

if [[ -z "$LAST_Q" ]]; then
  exit 1
fi

# A ===RESULT=== after the questions means the worker has moved on (done).
LAST_R=$(printf '%s\n' "$INPUT" | grep -n '^===RESULT===$' | tail -1 | cut -d: -f1 || true)
if [[ -n "$LAST_R" && "$LAST_R" -gt "$LAST_Q" ]]; then
  exit 1
fi

# The JSON should be on the line immediately after the marker.
JSON_LINE=$((LAST_Q + 1))
JSON="$(printf '%s\n' "$INPUT" | sed -n "${JSON_LINE}p" | sed 's/[[:space:]]*$//')"

if [[ -z "$JSON" ]]; then
  exit 2
fi

if command -v jq >/dev/null 2>&1; then
  if ! printf '%s' "$JSON" | jq -e . >/dev/null 2>&1; then
    exit 2
  fi
  if ! printf '%s' "$JSON" | jq -e '(.questions | type == "array") and (.questions | length > 0)' >/dev/null 2>&1; then
    exit 2
  fi
fi

printf '%s\n' "$JSON"
