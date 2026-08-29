#!/usr/bin/env bats
# Tests for the worker-session matching in flow-snapshot.sh / flow-summary.sh.
# Worker sessions are named "<title> · #<id>" (current) or "task-<id>[-…]"
# (legacy); both scripts must list them and tag them with their task #<id> for
# the **human board** (routing now goes through `message reply <id>`, which
# thurbox resolves — these scripts no longer drive it). A stub `thurbox-cli` on
# PATH feeds canned `session list` / `task list` JSON, so these run anywhere
# bats + jq are installed.

setup() {
  SNAPSHOT="${BATS_TEST_DIRNAME}/flow-snapshot.sh"
  SUMMARY="${BATS_TEST_DIRNAME}/flow-summary.sh"

  STUBDIR="$(mktemp -d)"
  # A worker per naming convention, the flow monitor, and a non-worker shell.
  cat >"${STUBDIR}/sessions.json" <<'JSON'
[
  {"name":"flow","id":"uuid-flow","agent":"flow","cwd":"/home/me/flow"},
  {"name":"Wire up SSH backend · #42","id":"uuid-42","agent":"flow-worker","cwd":"/repo"},
  {"name":"task-7-add-rate-limiting","id":"uuid-7","agent":"flow-worker","cwd":"/repo"},
  {"name":"task-9","id":"uuid-9","agent":"flow-worker","cwd":"/repo"},
  {"name":"Some random shell","id":"uuid-x","agent":"shell","cwd":"/x"}
]
JSON
  # #42 and #7 have live sessions; #4 is in_progress with NO session (detached);
  # #9 is done. #4 exercises the #4-vs-#42 boundary in the footer.
  cat >"${STUBDIR}/tasks.json" <<'JSON'
[
  {"id":42,"title":"Wire up SSH backend","status":"in_progress","created_at":1700000000000},
  {"id":7,"title":"Add rate limiting","status":"in_progress","created_at":1700000000000},
  {"id":4,"title":"Edit README","status":"in_progress","created_at":1700000000000},
  {"id":9,"title":"Done thing","status":"done","created_at":1700000000000}
]
JSON

  mkdir -p "${STUBDIR}/bin"
  # The stub answers like the real binary: JSON only when `--json` is passed,
  # and TOON otherwise (stdout here is a command substitution, i.e. a pipe).
  # A stub that answered JSON either way is why the scripts could drop `--json`
  # and still pass while every real caller silently rendered an empty section.
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
  # Not a faithful TOON encoder — just the shape that matters here: a tabular
  # answer that is not JSON, so a jq pipeline over it fails.
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

@test "summary syntax is valid" {
  run bash -n "$SUMMARY"
  [ "$status" -eq 0 ]
}

@test "snapshot's task board survives the pipe default (asks for --json)" {
  # Regression: the pipe default became TOON, and this section's jq pipeline
  # falls back to dumping the raw answer — so without `--json` the board
  # rendered as TOON instead of the grouped task list.
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  [[ "$output" == *"### in_progress"* ]]
  [[ "$output" == *"#42 Wire up SSH backend"* ]]
  [[ "$output" == *"### done"* ]]
  [[ "$output" != *"rows["* ]]
}

@test "summary board survives the pipe default (asks for --json)" {
  run "$SUMMARY"
  [ "$status" -eq 0 ]
  [[ "$output" != *"rows["* ]]
}

@test "snapshot lists current and legacy worker sessions, tagged with #<id>" {
  run "$SNAPSHOT"
  [ "$status" -eq 0 ]
  # Current "<title> · #<id>" worker — the case the bug used to drop.
  [[ "$output" == *"#42  Wire up SSH backend · #42  uuid-42"* ]]
  # Legacy "task-<id>-…" and bare "task-<id>" still resolve.
  [[ "$output" == *"#7  task-7-add-rate-limiting  uuid-7"* ]]
  [[ "$output" == *"#9  task-9  uuid-9"* ]]
  # The flow monitor is listed (untagged).
  [[ "$output" == *"—  flow  uuid-flow"* ]]
  # A non-worker session is excluded.
  [[ "$output" != *"Some random shell"* ]]
}

@test "summary board joins workers to their tasks" {
  run "$SUMMARY"
  [ "$status" -eq 0 ]
  [[ "$output" == *"#42"* ]]
  [[ "$output" == *"Wire up SSH backend"* ]]
  [[ "$output" == *"#7"* ]]
  [[ "$output" == *"Add rate limiting"* ]]
  [[ "$output" == *"monitor"* ]]          # the flow row
  [[ "$output" != *"Some random shell"* ]]
}

@test "summary footer flags only the detached task, honoring the #4 vs #42 boundary" {
  run "$SUMMARY"
  [ "$status" -eq 0 ]
  [[ "$output" == *"## detached"* ]]
  # Footer lines start with two spaces ("  #<id> <title>"), board rows start at
  # column 0 — so the two-space prefix targets the footer regardless of whether
  # `column` is installed (the no-column path collapses tabs to single spaces).
  # #4 has no session (the "· #42" session must NOT match id 4).
  [[ "$output" == *"  #4 Edit README"* ]]
  # #42 and #7 have live sessions; #9 is done — none are detached.
  [[ "$output" != *"  #42 Wire up SSH backend"* ]]
  [[ "$output" != *"  #7 Add rate limiting"* ]]
  [[ "$output" != *"  #9 Done thing"* ]]
}
