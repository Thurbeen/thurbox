#!/usr/bin/env bats
# forge-snapshot.sh driven end to end with a stub `thurbox-cli` on PATH that
# behaves the way the binary does: JSON only when asked, a tabular non-JSON
# shape otherwise.
#
# The failure this pins is not a wrong line but a truncated report: the
# automations section's `jq 'length'` has no fallback, so under
# `set -euo pipefail` a mis-parsed answer aborts the script and the sections
# below it — including "## open proposals" — never print at all.

setup() {
  SNAPSHOT="${BATS_TEST_DIRNAME}/forge-snapshot.sh"
  STUBDIR="$(mktemp -d)"

  cat >"${STUBDIR}/tasks.json" <<'JSON'
[
  {"id":7,"title":"batch the nightly sync","status":"todo","source":"local",
   "created_at":0,"action":{"kind":"spawn","agent":"claude","repo_path":"/repo"}}
]
JSON
  cat >"${STUBDIR}/sessions.json" <<'JSON'
[{"name":"forge","agent":"claude","cwd":"/repo"}]
JSON
  cat >"${STUBDIR}/automations.json" <<'JSON'
[{"id":3,"name":"forge-scan","enabled":true,"trigger":"weekly","prompt":"scan"}]
JSON
  cat >"${STUBDIR}/runs.json" <<'JSON'
[{"status":"success"},{"status":"success"},{"status":"failed"}]
JSON

  mkdir -p "${STUBDIR}/bin"
  cat >"${STUBDIR}/bin/thurbox-cli" <<EOF
#!/usr/bin/env bash
json=0
args=()
for a in "\$@"; do
  case "\$a" in
    --json|--pretty) json=1 ;;
    --*) ;;
    *) args+=("\$a") ;;
  esac
done
case "\${args[0]:-} \${args[1]:-}" in
  "task list")       file="${STUBDIR}/tasks.json" ;;
  "session list")    file="${STUBDIR}/sessions.json" ;;
  "automation list") file="${STUBDIR}/automations.json" ;;
  "automation runs") file="${STUBDIR}/runs.json" ;;
  *) file="" ;;
esac
if [ -z "\$file" ]; then
  [ "\$json" = 1 ] && echo "[]" || echo "rows[0]:"
  exit 0
fi
if [ "\$json" = 1 ]; then
  cat "\$file"
else
  jq -r 'length as \$n | "rows[\(\$n)]:", (.[] | "  -")' "\$file"
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

@test "every section renders its records, not the piped default's shape" {
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  [[ "$output" == *"#7 batch the nightly sync"* ]]
  [[ "$output" == *"forge  agent=claude  cwd=/repo"* ]]
  [[ "$output" == *"#3 forge-scan  enabled=true  weekly  runs[failed:1 success:2]"* ]]
  [[ "$output" != *"rows["* ]]
}

@test "the report is complete: an aborted section would swallow the ones below it" {
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  [[ "$output" == *"## tasks"* ]]
  [[ "$output" == *"## sessions"* ]]
  [[ "$output" == *"## automations (with recent run summary)"* ]]
  [[ "$output" == *"## open proposals"* ]]
}

@test "an unparseable automations answer degrades that section alone" {
  cat >"${STUBDIR}/bin/thurbox-cli" <<'EOF'
#!/usr/bin/env bash
case " $* " in
  *" automation list "*) echo "rows[1]{id}:"; echo "  3"; exit 0 ;;
esac
echo "[]"
EOF
  chmod +x "${STUBDIR}/bin/thurbox-cli"
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  [[ "$output" == *"unparseable answer"* ]]
  [[ "$output" == *"## open proposals"* ]]
}
