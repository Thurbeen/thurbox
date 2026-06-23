# gitlab-issues (thurbox extension)

> **Experimental.** Bidirectionally syncs **GitLab issues** with the thurbox
> task list: your issues show up as tasks, and marking a task done closes the
> issue.

A `gitlab-issues-tick` **automation** runs a deterministic sync script
(`scripts/sync.sh`) every 15 minutes — **no agent, no LLM, no tokens**. thurbox's
scheduler runs it (TUI or headless heartbeat) and records the result in the
automation run history. The script only calls `thurbox-cli` and the `glab` CLI.

## Setup

### 1. Prerequisites

- `thurbox-cli` **≥ 0.141** on `PATH` (needs the `Exec` automation action +
  `task --source/--external-id/--external-url`; check `thurbox-cli version`).
- `glab` (GitLab CLI) and `jq`.

### 2. Authenticate GitLab

```sh
glab auth login        # gitlab.com or your self-managed host; token needs the `api` scope
glab auth status       # confirm "Logged in"
```

No token env var is needed — `glab` stores the credential itself. (`GITLAB_HOST`
/ `GITLAB_TOKEN` also work if you prefer.)

### 3. Install the extension

```sh
thurbox-cli extension install gitlab-issues
# or from a checkout:
thurbox-cli extension install ./extensions/gitlab-issues
```

This lays down `~/.config/thurbox/extensions/gitlab-issues/` (override with
`--home`) and activates the `gitlab-issues-tick` automation, which thurbox
self-heals if deleted.

### 4. Configure the projects to sync

Edit `~/.config/thurbox/extensions/gitlab-issues/trackers.md` — one row per project or saved filter:

```markdown
| name    | query                            | push_back |
|---------|----------------------------------|-----------|
| backend | mygroup/backend --assignee=@me   | no        |
| triage  | mygroup/web --label=needs-triage | no        |
```

Keep `push_back = no` for the first run (pull only — nothing is changed on
GitLab).

### 5. First sync & verify

The automation fires every 15 min. To run it now, trigger it from the
**Automations** pane (`Ctrl+P` → select `gitlab-issues-tick` → `r`) or headless:

```sh
thurbox-cli automation run <id>     # id from: thurbox-cli automation list
# or run the script directly:
~/.config/thurbox/extensions/gitlab-issues/scripts/sync.sh
```

Then check the imported tasks:

```sh
thurbox-cli task list --json | jq -c '.[] | select(.source=="gitlab") | {id,status,external_id,title}'
```

Open issues import as `todo` (or `in_progress` if assigned), closed as `done`.
Re-running is **idempotent** — dedup by `(source, external_id)` means unchanged
issues are left alone. The run's output (created/updated/unchanged counts) shows
in the automation run history.

### 6. Enable two-way sync (optional)

Set `push_back = yes` on a tracker row. Now marking an imported task **done**
closes its GitLab issue, and moving it back to **todo** reopens it (only the
open/closed axis is pushed; `in_progress` stays open). The sync runs
**push-then-pull** so your local change reaches GitLab before the pull reads
state back.

## trackers.md reference

- **query** — `group/project` plus any `glab issue list` flags. Open issues are
  the default; pass `--all` to include closed, `--assignee=@me`, `--label=…`,
  `--milestone=…`, etc. (There is no `--state` flag for `glab issue list`.)
- **push_back** — `yes` enables thurbox → GitLab status push for that row.

Imported tasks carry `source=gitlab` and `external_id="group/project#<iid>"`, so
re-syncing never duplicates them.

## How it works

`scripts/sync.sh` (run by the automation) does, in order:

| Step | Script | Behavior |
|------|--------|----------|
| Push | `push-status.sh` | `done` → close, `todo`/`in_progress` → open (only `push_back=yes` rows) |
| Pull | `fetch.sh` + `upsert.sh` | List issues → create/update tasks; dedup by `(source, external_id)` |

GitLab is authoritative for an issue's title, URL, and open-vs-done state; your
local `todo`↔`in_progress` distinction is preserved.

## Troubleshooting

- **Nothing syncs** — check the automation run history (`Ctrl+P`, or
  `thurbox-cli automation runs <id>`) for the script's output; run
  `~/.config/thurbox/extensions/gitlab-issues/scripts/sync.sh` by hand to see errors directly.
- **`glab` auth error / 401 Unauthorized** — the stored token expired or lacks
  the `api` scope; re-run `glab auth login` and confirm with `glab auth status`.
- **Empty pull** — confirm the project path and that `glab issue list -R <path>`
  returns issues (default is open issues; add `--all`).
- **`unknown option '--source'`** — your `thurbox-cli` predates 0.141; rebuild /
  update thurbox.

## Turn it off

```sh
thurbox-cli extension deactivate gitlab-issues          # stop syncing
thurbox-cli extension uninstall gitlab-issues --purge   # remove home + automation
```

Imported tasks remain in your task list (they are not deleted on uninstall).
