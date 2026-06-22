# GitHub issue trackers

Each row maps a repo (or saved filter) to sync into the thurbox task list. The
`query` is `owner/repo` plus any `gh issue list` flags (`--state`, `--assignee`,
`--label`, `--milestone`); open issues are used when `--state` is omitted. Set
`push_back` to `yes` to push thurbox status changes back to GitHub (a task
marked done closes its issue; reopened when set back to todo) — leave it `no`
until you trust the sync. Imported tasks carry `source=github` and
`external_id="owner/repo#<number>"`. Authenticate first with `gh auth login`.

| name    | query                              | push_back |
|---------|------------------------------------|-----------|
| example | octocat/Hello-World --assignee @me | no        |
