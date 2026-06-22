#!/usr/bin/env bats
# Tests for upsert.sh — the dedup/reconciliation engine shared (byte-identical)
# by every task-integration extension (github-issues/gitlab-issues/linear/jira).
# Testing this one canonical copy covers them all.
#
# They mock `thurbox-cli` with a tiny fake on PATH: `task list` prints a canned
# "existing tasks" fixture, and `task create`/`task edit` append their args to a
# log the assertions inspect. Real `jq` is used. Run anywhere bats + jq exist:
#   bats extensions/github-issues/scripts/upsert.bats

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/upsert.sh"
  TMP="$(mktemp -d)"
  export MOCK_EXISTING="$TMP/existing.json" # what `task list --json` returns
  export MOCK_CALLS="$TMP/calls.log"        # create/edit calls land here
  echo '[]' >"$MOCK_EXISTING"
  : >"$MOCK_CALLS"
  mkdir -p "$TMP/bin"
  cat >"$TMP/bin/thurbox-cli" <<'SH'
#!/usr/bin/env bash
if [ "$1" = task ] && [ "$2" = list ]; then cat "$MOCK_EXISTING"; exit 0; fi
if [ "$1" = task ] && [ "$2" = create ]; then shift 2; printf 'create %s\n' "$*" >>"$MOCK_CALLS"; exit 0; fi
if [ "$1" = task ] && [ "$2" = edit ]; then shift 2; printf 'edit %s\n' "$*" >>"$MOCK_CALLS"; exit 0; fi
echo "unexpected: thurbox-cli $*" >&2; exit 99
SH
  chmod +x "$TMP/bin/thurbox-cli"
  PATH="$TMP/bin:$PATH"
}

teardown() { rm -rf "$TMP"; }

# Feed one incoming normalized issue (JSON array) through upsert. Invoked via
# `bash` since the repo copy isn't chmod +x (the manifest sets that on install).
upsert() { printf '%s' "$1" | bash "$SCRIPT" --source github; }

# A canned existing task (the `task list --json` shape) with overridable fields.
existing() {
  jq -nc --arg status "$1" --arg title "${2:-A}" \
    '[{ id: 5, title: $title, status: $status, source: "github",
        external_id: "o/r#1", external_url: "u" }]' >"$MOCK_EXISTING"
}

@test "syntax is valid" {
  run bash -n "$SCRIPT"
  [ "$status" -eq 0 ]
}

@test "requires --source" {
  run bash -c "echo '[]' | bash '$SCRIPT'"
  [ "$status" -ne 0 ]
}

@test "a new issue is created with the external-sync flags" {
  run upsert '[{"external_id":"o/r#1","title":"A","status":"todo","url":"u","description":""}]'
  [ "$status" -eq 0 ]
  [[ "$output" == *"created=1 updated=0 unchanged=0"* ]]
  grep -q -- "--source github" "$MOCK_CALLS"
  grep -q -- "--external-id o/r#1" "$MOCK_CALLS"
}

@test "an identical issue is unchanged (dedup, no write)" {
  existing todo
  run upsert '[{"external_id":"o/r#1","title":"A","status":"todo","url":"u","description":""}]'
  [[ "$output" == *"created=0 updated=0 unchanged=1"* ]]
  [ ! -s "$MOCK_CALLS" ] # no create/edit happened
}

@test "a changed title triggers an edit" {
  existing todo OLD
  run upsert '[{"external_id":"o/r#1","title":"NEW","status":"todo","url":"u","description":""}]'
  [[ "$output" == *"created=0 updated=1 unchanged=0"* ]]
  grep -q "^edit 5 .*--title NEW" "$MOCK_CALLS"
}

@test "local in_progress is preserved when the remote is still open (todo)" {
  existing in_progress
  run upsert '[{"external_id":"o/r#1","title":"A","status":"todo","url":"u","description":""}]'
  [[ "$output" == *"unchanged=1"* ]]
  [ ! -s "$MOCK_CALLS" ] # in_progress is NOT clobbered back to todo
}

@test "a remote completion is pulled in (done)" {
  existing in_progress
  run upsert '[{"external_id":"o/r#1","title":"A","status":"done","url":"u","description":""}]'
  [[ "$output" == *"updated=1"* ]]
  grep -q "^edit 5 .*--status done" "$MOCK_CALLS"
}

@test "a remote reopen drops a done task back to todo" {
  existing done
  run upsert '[{"external_id":"o/r#1","title":"A","status":"todo","url":"u","description":""}]'
  [[ "$output" == *"updated=1"* ]]
  grep -q "^edit 5 .*--status todo" "$MOCK_CALLS"
}
