# Repo routing table

Used by the flow agent to map brain-dump keywords to repositories.
Add one row per repo you work on; the flow agent appends rows as it
learns new paths.

| name | path | base | keywords |
|------|------|------|----------|
| example | /home/me/repositories/example | main | example, sample |

A dump that spans several of these rows becomes ONE **multi-repo** task: flow
passes the most-central repo as `--repo` and each other as `--add-repo
<path>@origin/<base>`, so every repo gets its own isolated `flow/<slug>`
worktree and the worker opens a separate PR per repo it changes. See FLOW.md
("Multi-repo tasks").
