#!/usr/bin/env bats
# The "## update tasks" section of renovate-snapshot.sh, driven through the real
# script with a stub `thurbox-cli` on PATH.
#
# The section's jq pipeline ends in `2>/dev/null || true`, so a mis-parsed
# answer renders as an EMPTY section with no error — every updater task hidden
# from the monitoring agent, silently. That is the failure this pins: the stub
# answers JSON only for `--json` and a tabular non-JSON shape otherwise, the
# way the binary does down a pipe.

setup() {
  SNAPSHOT="${BATS_TEST_DIRNAME}/renovate-snapshot.sh"
  STUBDIR="$(mktemp -d)"

  cat >"${STUBDIR}/tasks.json" <<'JSON'
[
  {"id":21,"title":"update web-app deps (minor)","status":"in_progress","action":{"kind":"spawn","repo_path":"/repo"}},
  {"id":22,"title":"unrelated chore","status":"todo","action":{"kind":"spawn","repo_path":"/other"}}
]
JSON
  echo '[]' >"${STUBDIR}/sessions.json"

  mkdir -p "${STUBDIR}/bin"
  cat >"${STUBDIR}/bin/thurbox-cli" <<EOF
#!/usr/bin/env bash
json=0
args=()
for a in "\$@"; do
  case "\$a" in
    --json|--pretty) json=1 ;;
    *) args+=("\$a") ;;
  esac
done
case "\${args[0]:-} \${args[1]:-}" in
  "task list")    file="${STUBDIR}/tasks.json" ;;
  "session list") file="${STUBDIR}/sessions.json" ;;
  *) file="" ;;
esac
if [ -z "\$file" ]; then
  [ "\$json" = 1 ] && echo "[]" || echo "rows[0]:"
  exit 0
fi
if [ "\$json" = 1 ]; then
  cat "\$file"
else
  jq -r 'length as \$n | "rows[\(\$n)]{id}:", (.[] | "  \(.id)")' "\$file"
fi
EOF
  chmod +x "${STUBDIR}/bin/thurbox-cli"
  PATH="${STUBDIR}/bin:${PATH}"
}

teardown() {
  rm -rf "${STUBDIR}"
}

@test "snapshot syntax is valid" {
  run bash -n "$SNAPSHOT"
  [ "$status" -eq 0 ]
}

@test "update tasks are listed, not silently dropped by the pipe default" {
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  [[ "$output" == *"## update tasks"* ]]
  [[ "$output" == *"#21 [in_progress] update web-app deps (minor)"* ]]
  # Only `update …` titles belong to the renovate monitor.
  [[ "$output" != *"unrelated chore"* ]]
  [[ "$output" != *"rows["* ]]
}
