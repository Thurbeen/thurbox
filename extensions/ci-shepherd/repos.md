# Change-request watch list

The shepherd polls these repos for open change requests (PRs/MRs) that need
work. Add one row per repo. `author` selects whose requests to watch (default
`@me` — your own; set a username, or `*` for everyone's). `provider` is the
forge: `github`, `gitlab`, `bitbucket`, or `auto` (detect from the git remote).
Any other git forge works too — leave `provider` as `auto` and the shepherd
agent figures out how to talk to it.

| name | path | author | provider |
|------|------|--------|----------|
| example | /home/me/repositories/example | @me | auto |
