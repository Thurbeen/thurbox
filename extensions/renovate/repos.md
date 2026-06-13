# Dependency-update watch list

The renovate monitor sweeps these repos on its schedule and dispatches a worker
to update each one's dependencies. Add one row per repo.

- `path` — absolute path to the local git checkout.
- `strategy` — how far to bump versions: `patch`, `minor` (patch + minor, the
  default), `major`, or `all` (everything, same as `major`). Tune the global
  behaviour (grouping, lockfile maintenance, ignored deps) in
  `renovate-config.json`; this column is the per-repo override.
- `provider` — the forge a worker opens its review PR against: `github`,
  `gitlab`, `bitbucket`, or `auto` (detect from the git remote). Renovate itself
  never talks to the forge — it only runs locally; the worker pushes the branch
  and opens the PR. Leave `auto` and the worker figures the forge out.

| name | path | strategy | provider |
|------|------|----------|----------|
| example | /home/me/repositories/example | minor | auto |
