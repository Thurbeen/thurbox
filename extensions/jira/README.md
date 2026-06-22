# jira (thurbox extension)

> **Experimental.** Bidirectionally syncs **Jira issues** with the thurbox task
> list: your issues show up as tasks, and marking a task done transitions the
> issue into the Done category.

A `jira-tick` **automation** runs a deterministic sync script (`scripts/sync.sh`)
every 15 minutes — **no agent, no LLM, no tokens**. thurbox's scheduler runs it
(TUI or headless heartbeat) and records the result in the automation run
history. The script only calls `thurbox-cli`, `curl`, and `jq` against the Jira
Cloud REST API.

## Setup

### 1. Prerequisites

- `thurbox-cli` **≥ 0.141** on `PATH` (needs the `Exec` automation action +
  `task --source/--external-id/--external-url`; check `thurbox-cli version`).
- `curl` and `jq`.

### 2. Create a Jira API token & credentials.env

Create an Atlassian API token at
<https://id.atlassian.com/manage-profile/security/api-tokens>.

Because the sync runs **headless** (via the automation, with no shell session to
inherit your environment), the robust way to hand it the credentials is a
`credentials.env` file in the extension home — `sync.sh` sources
`~/jira/credentials.env` if present. Create it with the three required vars:

```sh
mkdir -p ~/jira
cat > ~/jira/credentials.env <<'EOF'
JIRA_BASE_URL=https://your-domain.atlassian.net
JIRA_EMAIL=you@example.com
JIRA_API_TOKEN=your-atlassian-api-token
EOF
chmod 600 ~/jira/credentials.env
```

Alternatively you can `export` those three vars from your shell profile, but that
only reaches the script when the profile's environment runs it —
`credentials.env` is the reliable path for the headless automation, so it is
recommended.

### 3. Install the extension

```sh
thurbox-cli extension install jira
# or from a checkout:
thurbox-cli extension install ./extensions/jira
```

This lays down `~/jira/` (override with `--home`) and activates the `jira-tick`
automation, which thurbox self-heals if deleted.

### 4. Configure the projects/filters to sync

Edit `~/jira/trackers.md` — one row per project or saved filter. Each row's
`query` is a **JQL string** (it may contain spaces and is used verbatim):

```markdown
| name    | query                                    | push_back |
|---------|------------------------------------------|-----------|
| eng     | project = ENG AND statusCategory != Done | no        |
| triage  | project = WEB AND labels = needs-triage  | no        |
```

Keep `push_back = no` for the first run (pull only — nothing is changed on Jira).

### 5. First sync & verify

The automation fires every 15 min. To run it now, trigger it from the
**Automations** pane (`Ctrl+P` → select `jira-tick` → `r`) or headless:

```sh
thurbox-cli automation run <id>     # id from: thurbox-cli automation list
# or run the script directly:
~/jira/scripts/sync.sh
```

Then check the imported tasks:

```sh
thurbox-cli task list --json | jq -c '.[] | select(.source=="jira") | {id,status,external_id,title}'
```

Issues import as `todo`/`in_progress`/`done` by their Jira status category.
Re-running is **idempotent** — dedup by `(source, external_id)` means unchanged
issues are left alone. The run's output (created/updated/unchanged counts) shows
in the automation run history.

### 6. Enable two-way sync (optional)

Set `push_back = yes` on a tracker row. Now marking an imported task **done**
transitions its Jira issue into the Done category, and moving it back to **todo**
reopens it (only the open-vs-done axis is pushed; `in_progress` stays open). The
sync runs **push-then-pull** so your local change reaches Jira before the pull
reads state back.

Note push-back is **all-or-nothing across jira tasks**: Jira issue keys can't be
tied back to a specific JQL tracker row, so push-back is gated on "at least one
`trackers.md` row has `push_back = yes`" — and when so, it acts on **all**
`source=jira` tasks. Leave every row `no` to disable push entirely.

## trackers.md reference

- **query** — a **JQL string**, used verbatim (e.g.
  `project = ENG AND statusCategory != Done`). It may contain spaces; it is not
  word-split.
- **push_back** — `yes` enables thurbox → Jira status push (all-or-nothing
  across jira tasks, see above).

Imported tasks carry `source=jira` and `external_id="<issue key>"` (e.g.
`ENG-7`), so re-syncing never duplicates them.

## How it works

`scripts/sync.sh` (run by the automation) does, in order:

| Step | Script | Behavior |
|------|--------|----------|
| Push | `push-status.sh` | `done` → transition to Done, `todo`/`in_progress` → reopen (gated on any `push_back=yes` row) |
| Pull | `fetch.sh` + `upsert.sh` | List issues via JQL → create/update tasks; dedup by `(source, external_id)` |

Notes:

- This is a **deterministic automation** — no agent, no LLM, no tokens.
- The fetch uses Jira Cloud's current `/rest/api/3/search/jql` endpoint (the old
  `/rest/api/3/search` was removed and returns HTTP 410).
- **Descriptions are not synced** — Jira descriptions are ADF (rich JSON);
  converting them to text is out of scope, so the task description stays blank.
- **Status maps by `statusCategory`**: `new` → `todo`, `indeterminate` →
  `in_progress`, `done` → `done`.
- **Push uses workflow transitions**: a workflow that lacks a transition into
  the wanted category is **surfaced** in the output (not a failure) and skipped.
- Jira is authoritative for an issue's title, URL, and open-vs-done state; your
  local `todo`↔`in_progress` distinction is preserved.

## Troubleshooting

- **Nothing syncs** — check the automation run history (`Ctrl+P`, or
  `thurbox-cli automation runs <id>`) for the script's output; run
  `~/jira/scripts/sync.sh` by hand to see errors directly.
- **`set JIRA_BASE_URL …` / auth errors** — the headless run can't see the vars;
  confirm `~/jira/credentials.env` exists with all three (`JIRA_BASE_URL`,
  `JIRA_EMAIL`, `JIRA_API_TOKEN`). Test with
  `curl -u "$JIRA_EMAIL:$JIRA_API_TOKEN" "$JIRA_BASE_URL/rest/api/3/myself"`.
- **`unknown option '--source'`** — your `thurbox-cli` predates 0.141; rebuild /
  update thurbox.

## Turn it off

```sh
thurbox-cli extension deactivate jira          # stop syncing
thurbox-cli extension uninstall jira --purge   # remove home + automation
```

Imported tasks remain in your task list (they are not deleted on uninstall).
