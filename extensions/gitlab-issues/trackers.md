# GitLab issue trackers

Each row maps a project (or saved filter) to sync into the thurbox task list.
The `query` is `group/project` plus any `glab issue list` flags — open issues by
default, `--all` to include closed, plus `--assignee=@me`, `--label=…`,
`--milestone=…`. Set `push_back` to `yes` to push thurbox status changes back to
GitLab (a task marked done closes its issue; reopened when set back to todo) —
leave it `no` until you trust the sync. Imported tasks carry `source=gitlab` and
`external_id="group/project#<iid>"`. Authenticate first with `glab auth login`.

| name    | query                                 | push_back |
|---------|---------------------------------------|-----------|
| example | gitlab-org/gitlab-foss --assignee=@me | no        |
