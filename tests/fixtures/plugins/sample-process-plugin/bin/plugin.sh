#!/usr/bin/env sh
# Minimal sample process plugin used by the runtime integration test.
#
# Reads one JSON-RPC request per line on stdin and emits one response per
# line on stdout. Recognised ops: `handshake`, `stop`. Anything else returns
# `{ok:false, error:"unknown op"}`.
#
# Diagnostic prints go to stderr — thurbox captures them into the per-plugin
# log file so the operator can see what the plugin did.

set -e

echo "sample-process-plugin started" >&2

while IFS= read -r line; do
    [ -z "$line" ] && continue

    # Extract id and op via dumb sed — sufficient for one-line JSON requests.
    id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p')
    op=$(printf '%s' "$line" | sed -n 's/.*"op":[[:space:]]*"\([^"]*\)".*/\1/p')

    case "$op" in
        handshake)
            printf '{"id":%s,"ok":true,"result":{"api_version":1,"capabilities":["mcp-tools"]}}\n' "$id"
            echo "handshake completed (id=$id)" >&2
            ;;
        stop)
            echo "stop received; exiting" >&2
            exit 0
            ;;
        *)
            printf '{"id":%s,"ok":false,"error":"unknown op: %s"}\n' "$id" "$op"
            echo "unknown op: $op (id=$id)" >&2
            ;;
    esac
done

echo "sample-process-plugin: stdin closed, exiting" >&2
