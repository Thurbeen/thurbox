## 1. Data (`session`)

- [x] 1.1 Add `session::hook_def` with `HookEvent` (closed enum, serde-renamed to the `session.pre_create` … `session.post_restore` spellings), `LifecycleHook { event, command, timeout_secs }` and `HooksFile { hooks }` with `deny_unknown_fields`
- [x] 1.2 Add `HookContext` (event, session id, name, agent, repo, cwd, branch, base branch, host, parent session, task, agent session id, force / force_deleted, backend id, worktrees, additional dirs) with `env()` (unset-not-empty for unknown facts), `json()` and `workdir()` (primary repo if a local directory, else none)
- [x] 1.3 Unit tests: every event round-trips through serde by its dotted name, an unknown event and an unknown field fail to parse, `env()` sets exactly the documented `THURBOX_*` names, `json()` carries `event` and `worktrees`, `workdir()` falls back for a remote path

## 2. Config (`agent`)

- [x] 2.1 Add `agent::hooks_config` mirroring `host_config`: `hooks_config_path()`, a commented-out seed whose header states the difference from the `hooks/` extension home and shows one entry per pre/post pair, `load_or_seed_with_warnings()`, `load_or_seed()`, `hooks_for(event)` in file order
- [x] 2.2 Unit tests: the seed parses to zero hooks and documents every field and every event name, a missing file is seeded, a malformed file degrades to no hooks with a warning naming `hooks.toml`, a two-entry file yields them in order

## 3. Runner (`session_ops`)

- [x] 3.1 Extract from `run_exec_command` a platform-shell command builder and make `exec_tail` shared; `run_exec_command` keeps its signature and behaviour
- [x] 3.2 Lift the `THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR` override computation out of `inject_thurbox_env` into a helper both it and the hook runner use
- [x] 3.3 Add `session_ops::lifecycle_hooks` with `run_hook(hook, ctx) -> Result<(), String>`: env from `ctx.env()` plus the overrides, cwd from `ctx.workdir()`, JSON on a piped stdin closed before waiting, stdout/stderr drained on reader threads, `try_wait` polling against the hook's timeout (default 30 s), `kill()` and a `timed out after Ns` message on the deadline, `exit N: <stderr tail>` on a non-zero exit
- [x] 3.4 Add `fire_pre(event, &ctx) -> Result<(), String>` (first failure aborts, later hooks do not run, failure logged) and `fire_post(event, &ctx) -> Vec<String>` (all run, every failure collected and logged at warn); both read `hooks.toml` at call time
- [x] 3.5 Unit tests (unix): a hook that echoes `$THURBOX_HOOK_EVENT` and its stdin to a file sees the right values, exit 3 with stderr surfaces as `exit 3: …`, `sleep 5` with `timeout_secs = 1` is killed and reported as timed out, `yes` with a timeout does not deadlock, two pre-hooks stop at the first failure, two post-hooks both run past a failure

## 4. Firing (`session_ops` pipelines)

- [x] 4.1 `spawn`: mint the `SessionId` before validation, add `SpawnPhase::Hooks` reported before `fire_pre(PreCreate)` (after name/parent/host validation, before `resolve_dirs`), `fire_post(PostCreate)` after the row and base branch are persisted, `hook_failures` on `SpawnResult`
- [x] 4.2 `delete`: `fire_pre(PreDelete)` after the row is loaded and before any teardown, `fire_post(PostDelete)` after the soft delete (+ force mark), `hook_failures` on `ForceDeleteReport`; a veto returns `Err` with nothing changed
- [x] 4.3 `restart`: `fire_pre(PreRestart)` after the plan is built and before the kill, `fire_post(PostRestart)` after the new pane id is persisted; return a `RestartReport { hook_failures }` and update its callers (`cli::sessions`, `kernel::command::execute`)
- [x] 4.4 `restore`: `fire_pre(PreRestore)` after the best-effort and remote refusals and before `db.restore_session`, `fire_post(PostRestore)` after `respawn`, `hook_failures` on `RestoreReport`
- [x] 4.5 Add `"hooks"` to the pinned phase sequence in `tests/kernel_mvp.rs` and a `running hooks` label to `PHASE_LABEL` in `ui/plugins/10_sessions.lua`; run `selene`/`stylua`/`lua-language-server` on the Lua

## 5. CLI surface

- [x] 5.1 `cli::config validate`: strict-parse `hooks.toml` through the same `validate_toml` path as `hosts.toml`, add it to the per-file pass/fail list and the JSON
- [x] 5.2 `cli::config show`: print `hooks_toml` path and the hooks in force (event + command + timeout)
- [x] 5.3 `cli::sessions`: emit `hook_failures` in the JSON of `create`, `delete`, `restart`, `restore`; `cli::action`'s automation spawn logs them
- [x] 5.4 Tests in `cli::config`: validate fails and names `hooks.toml` on an unknown field; show lists two declared hooks

## 6. End-to-end proof

- [x] 6.1 In `tests/create_e2e.rs` (tmux, unix): with `THURBOX_CONFIG_DIR` pointing at a tempdir holding a `hooks.toml`, a headless create fires `pre_create` then `post_create` once each, in that order, with `THURBOX_CWD` = the worktree and `THURBOX_REPO` = the repository, and the hook's `thurbox-cli session get $THURBOX_SESSION` (dev binary on PATH) finds the row
- [x] 6.2 A `pre_create` hook exiting 1 leaves no worktree, no tmux window and no row, and the error carries its stderr
- [x] 6.3 A `post_create` hook exiting 2 leaves the session running and the failure in `SpawnResult.hook_failures`
- [x] 6.4 A TUI creation through the command bus (`tests/kernel_mvp.rs` style) reports the `hooks` phase and a veto as the in-flight error
- [x] 6.5 Delete (soft and force), restart and restore each fire their pair once; a `pre_delete` veto leaves the row undeleted

## 7. Docs

- [x] 7.1 `docs/CONFIG.md`: a `hooks.toml` section (file layout, the eight events, veto vs. informational, the env table, the stdin JSON, cwd rule, timeout, remote sessions), one sentence distinguishing it from the `hooks/` extension home, the new `THURBOX_*` names in the environment-variables table, `config validate`/`show` coverage
- [x] 7.2 `docs/FEATURES.md`: a Session lifecycle hooks entry
- [x] 7.3 `CLAUDE.md`: a short Session lifecycle hooks section under the session sections, naming the modules and the single-pipeline reason hooks fire once per caller; list `hooks.toml` where the config files are enumerated
- [x] 7.4 `just lint` clean (rumdl on the docs, clippy, fmt, the three Lua gates) and `cargo nextest run --all` green
