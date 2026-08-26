## Context

See `proposal.md` — Why. The requirements are in `specs/session-hooks/spec.md`.

What makes this small is a property the codebase already has: each session
operation has exactly one pipeline, and every interface goes through it.
Creation is `session_ops::spawn::spawn_session_headless_with_progress` (the
TUI's `kernel::command::execute::create`, the fork path, `thurbox-cli session
create`, `cli::action`'s automation spawn and an extension's session self-heal
all end there); deletion is `session_ops::delete_session_headless` (the TUI's
`Command::Delete` for both the soft and the hard path, the CLI, extension
uninstall); restart is `restart_session_headless`; restore is
`restore_session_headless`, which the TUI's undo also dispatches. So a hook
placed inside those four functions fires once per operation for every caller,
and there is nothing to add in the kernel or the interface.

Constraints that shape the rest:

- **Rule 5** — anything touching the world runs on a worker. The four functions
  already do (the command worker and the spawn worker in the TUI; the main
  thread in the CLI), so a hook that runs synchronously *inside* them inherits
  that for free and adds no thread.
- **Architecture rules** (`tests/architecture_rules.rs`) — `session` is pure
  data, `agent` loads config and may not touch `git`, `session_ops` may reach
  both. Rules are per top-level module, so new submodules need no new entries.
- **The TUI owns the terminal.** A child process that inherits stdout paints
  over the interface; one that inherits stdin steals keystrokes. `Exec`
  automations already solve this in `session_ops::run_exec_command` by
  capturing both and tail-truncating.
- **`hooks` is a taken word.** `session_ops::builtin_hooks`, `hooks_enabled`
  and the `<config>/hooks/` directory are the *agent status hook* extension —
  files thurbox installs into the agent CLIs. This change is the reverse
  direction and must not be confused with it in code or docs.

## Goals / Non-Goals

**Goals:**

- Declarative, agent-neutral, zero-cost when unused.
- Veto semantics for `pre_*`, informational for `post_*` — git's model, which
  is what anyone who has written a `pre-commit` expects.
- One runner, one context builder, one place the environment convention lives.

**Non-Goals:**

- **Failure events** (`session.create_failed`). A `post_*` hook fires only on
  success; a user who needs cleanup after a vetoed or failed creation has no
  event for it yet. Deliberately left out: it doubles the event set for the
  rarest case, and the shape here (one runner, one context) makes it a later
  addition of one variant and one call.
- **Events for the reaper, sync, reorder, send, rename.** The reaper is the
  tail of a soft delete, not an operation of its own; the rest are not
  lifecycle.
- **Hooks in extension manifests** (`[[hooks]]` in `extension.toml`). The
  manifest machinery could carry them the way it carries `[[automations]]`;
  that is a second change once the file format has been used.
- **Running a hook on the session's remote host.** Hooks run where thurbox
  runs. `THURBOX_HOST` tells a hook the session is elsewhere; `ssh $HOST …`
  from the hook is the user's call.
- **A TUI surface for hook failures beyond the log.** A post-hook failure is
  in `thurbox.log` and in the CLI's JSON; toasting it in the interface is a
  follow-up on the command bus's result channel, not part of this.
- **A Lua-side hook API.** Plugins already have `command`; a plugin that wants
  to react to session creation reads the snapshot. This is for the user's
  scripts, not for panes.

## Decisions

### D1 — A dedicated `hooks.toml`, read at fire time, not a `settings.toml` section

`agents.toml`, `hosts.toml`, `themes.toml` and `plugins.toml` are each a
registry of `[[entries]]` in their own file, loaded by an `agent::*_config`
module with a commented-out seed and a `load_or_seed_with_warnings` that
degrades to empty on a parse error. Hooks are the same shape, so they get the
same treatment: `agent::hooks_config` mirrors `host_config` line for line
(path via `paths::config_file().with_file_name("hooks.toml")`, seed, strict
parse through `parse_toml_reporting_unknown`, empty-with-warning on failure).

The file is read **each time an event fires**, on the worker that fires it.
That is a small disk read per session operation — a rate measured in
operations per minute at most — and it buys: no cache to give an age to
(ADR-P13/P18 both bit this codebase before), no live-reload plumbing, an edit
in force the next time a hook fires, and nothing held in `App`.

*Alternative considered.* A `[[hooks]]` array in `settings.toml`. Rejected:
`settings.toml` is round-tripped by the settings panel through `toml_edit`
and live-reloaded through `Config::adopt`/`restart_only_differs`, and hooks
would be the one section neither the panel offers nor the adopt logic cares
about — present in that file only to be stepped around.

*The name.* `hooks.toml` will sit beside the `hooks/` directory that is the
status-hook extension's home. The two are the two directions of the same
word — what thurbox runs, what thurbox installs — and the alternative names
(`events.toml`, `lifecycle.toml`) are less discoverable to someone who arrives
asking "does thurbox have hooks?". The docs draw the line in one sentence at
the top of the `hooks.toml` section; the code never says bare "hooks" for
this feature (`lifecycle_hooks`, `HookEvent`, `LifecycleHook`).

### D2 — Data in `session`, loading in `agent`, running in `session_ops`

- `session::hook_def` — `HookEvent` (a closed enum with a serde rename to the
  `session.pre_create` spelling, so an unknown event is a parse error rather
  than a silently-never-fired hook), `LifecycleHook { event, command,
  timeout_secs: Option<u64> }`, `HooksFile { hooks: Vec<LifecycleHook> }`
  with `deny_unknown_fields`, and `HookContext` — the facts handed to a hook,
  with `fn env(&self) -> Vec<(String, String)>` and `fn json(&self) ->
  String`. Pure data plus formatting, which is what `session` is for, and
  what lets the env convention be unit-tested without a process.
- `agent::hooks_config` — the file: path, seed, `load_or_seed_with_warnings`,
  `hooks_for(event) -> Vec<LifecycleHook>` preserving file order.
- `session_ops::lifecycle_hooks` — the runner. `fire_pre(event, &ctx) ->
  Result<(), String>` (stops at the first failure, returns its message);
  `fire_post(event, &ctx) -> Vec<String>` (runs all, returns every failure's
  message). Both log; `fire_post` logs at `warn`.

### D3 — The runner: `sh -c`, captured output, a piped stdin, a real timeout

`run_exec_command` already has the right platform split and the tail helper
(`exec_tail`). It cannot be reused as is: it has no env, no stdin and no
timeout. The runner extracts the shared parts (`platform shell + args`,
`exec_tail`) so `Exec` automations and hooks build the command the same way,
then adds:

- `envs(ctx.env())` on top of the inherited environment — plus the two
  override vars `inject_thurbox_env` already computes for agents
  (`THURBOX_CONFIG_DIR`, `THURBOX_DATA_DIR`), lifted into a helper the two
  share so the "a `thurbox-cli` inside hits the right DB" property has one
  definition.
- `stdin(Stdio::piped())`, the JSON written and the handle dropped before
  waiting; `stdout`/`stderr` piped. Never inherited.
- A timeout, default 30 s. `std::process::Child` has no timed wait, so the
  runner polls `try_wait` at a short interval while draining the pipes on
  two reader threads (draining is what stops a chatty hook from blocking on
  a full pipe — the reason `output()` could not simply be given a deadline),
  and on the deadline `kill()`s the child and reports `timed out after Ns`.
  Nothing async: the callers are synchronous functions on a worker, and
  `session_ops` has no runtime to lean on.

*Alternative considered.* `tokio::process` with `timeout`. Rejected: `session_ops`
is synchronous throughout and is called from the CLI's main thread as well as
the TUI's workers; entering a runtime from a worker that may itself be inside
one is the trap `kernel::terminal` documents.

### D4 — Where each event fires, and what the context knows

| Event | Fires at | Context notes |
|---|---|---|
| `pre_create` | after `validate_safe_name`/`validate_parent_session` and host resolution, before `resolve_dirs` | the `SessionId` is minted **before** this point (it is a fresh UUID, minting it earlier changes nothing) so the hook can correlate with `post_create`; `THURBOX_CWD` is unset — the worktree path is not known until it is made |
| `post_create` | after the row is persisted and the base branch recorded, before returning | full context: id, cwd, worktrees, backend id |
| `pre_delete` | after the row is loaded, before `teardown_runtime_resources` | `force` in the payload |
| `post_delete` | after `soft_delete_session` (+ `mark_session_force_deleted`) | the worktrees list is what *was* there |
| `pre_restart` | after the plan is built, before the kill | — |
| `post_restart` | after the new pane id is persisted | `backend_id` is the new pane |
| `pre_restore` | after the best-effort and remote refusals, before `db.restore_session` | `force_deleted` in the payload |
| `post_restore` | after `respawn`, whatever it returned | a restore whose agent did not come up is still a restore (the report says so) |

`SpawnPhase` gains `Hooks` ("hooks"), reported before `fire_pre`, so the
placeholder row says `running hooks` rather than `resolving` while a slow one
runs. `tests/kernel_mvp.rs` pins the phase sequence and
`ui/plugins/10_sessions.lua`'s `PHASE_LABEL` maps names to labels — both grow
one entry.

The working directory rule ("the primary repository if it is a local
directory, else thurbox's own") is chosen because the primary repository is
the one path that exists at every event: at `pre_create` the worktree is not
made, at `post_delete` it is gone. `HookContext::workdir()` owns the rule.

### D5 — Reporting a post-hook failure

`SpawnResult`, `ForceDeleteReport` and `RestoreReport` gain
`hook_failures: Vec<String>`; `restart_session_headless` returns `()` today
and gains a small `RestartReport { hook_failures }` rather than a bool. The
CLI JSON emits the field; the TUI's command worker logs each entry (its
result channel carries only success/error today, and widening it is the
non-goal above). A `pre_*` veto is an `Err(String)` from the operation, which
already reaches the in-flight command's error and the CLI's exit status.

### D6 — Environment variable names extend the existing convention

`THURBOX_SESSION`, `THURBOX_TASK`, `THURBOX_CONFIG_DIR`, `THURBOX_DATA_DIR`
already mean what they will mean inside a hook — the injected identity an
agent gets. The new names (`THURBOX_HOOK_EVENT`, `THURBOX_SESSION_NAME`,
`THURBOX_AGENT`, `THURBOX_REPO`, `THURBOX_CWD`, `THURBOX_BRANCH`,
`THURBOX_BASE_BRANCH`, `THURBOX_HOST`, `THURBOX_PARENT_SESSION`) follow the
prefix and are documented in `docs/CONFIG.md`'s environment-variables table
beside the existing ones. `THURBOX_SESSION_ID` (the *agent* session id the
metrics statusline reads) is set too, for symmetry with the agent's
environment. A variable is **unset**, not empty, when the fact is unknown, so
`${THURBOX_HOST:+…}` idioms work.

## Risks / Trade-offs

- **A pre-hook makes every creation slower, and a hung one makes it fail.** →
  The timeout is per hook (default 30 s) and always enforced; the phase is
  named so the user sees *hooks* and not a mysterious stall. Nothing else in
  the pipeline waits on a hook.
- **A hook runs with the user's authority and thurbox spawns it unasked.** →
  Same position as `run` capabilities and `Exec` automations: the user wrote
  the file; thurbox can only refuse to run things it was not asked to run. The
  file lives in the config dir, not in a repository, so a cloned repo cannot
  bring a hook with it.
- **Two hooks vocabularies in one project.** → Code never says bare "hooks"
  for this; docs draw the line once, where the reader will look
  (`docs/CONFIG.md`, the seed file's header comment).
- **Reading the file per fire hides a broken file until the next operation.** →
  `thurbox-cli config validate` catches it on demand, and the fire-time warning
  is logged with the parse error; a malformed file never blocks an operation,
  it only means no hooks ran — the same degrade `hosts.toml` chose.
- **The reader-thread timeout is more code than `output()`.** → It is the only
  correct shape: a deadline without draining deadlocks on a full pipe; draining
  without a deadline hangs on a hook that never exits. Unit-tested with `sleep`
  and `yes`.
- **`post_*` failures in the TUI are log-only.** → Accepted for this change;
  the report field exists so the interface can show it once the command bus
  carries warnings.

## Migration Plan

Nothing to migrate: the file is seeded commented-out, the reports' new fields
are additive (`Vec` defaults to empty in JSON), and no schema or setting
changes. Rollback is deleting the file's entries.

## Findings from implementing

**`deny_unknown_fields` was dropped (D2, task 1.1).** Every other config file
tolerates an unknown key with a named warning at load and fails only
`config validate` on it (`parse_toml_reporting_unknown`), and the seed files
promise exactly that. A hooks file that refused to load over a misspelt
`timeout_sec` would be the one file that broke the convention, and the
scenario it protects — "a misspelt field fails `validate`" — holds either
way. `HooksFile` follows the convention; an unknown *event* is still a parse
error, because that is a variant of a closed enum, not a field.

**The CLI's `session restore` did not run the restore pipeline.** It cleared
the row's flags and stopped — no worktrees, no agent — even though
`restore_session_headless` exists "so the interface and the command line
cannot disagree about what restoring means". The spec needs the restore hooks
to fire from the CLI, so `cli::sessions::restore_deleted` now calls the
pipeline; its JSON gains `worktrees_wanted`/`worktrees_recovered`/
`respawn_error` beside `hook_failures`. A behaviour fix in its own right,
recorded here because it is wider than the change's title.

**The overrides stayed out of `Exec` automations.** `platform_shell` is shared
with `run_exec_command`, and the first cut put the `THURBOX_CONFIG_DIR`/
`THURBOX_DATA_DIR` overrides inside it — which would have changed every `Exec`
automation's environment as a side effect of a hooks change. They live in the
hook runner instead; `run_exec_command` is byte-for-byte what it was.

**The stdin write is a thread too (D3).** A hook that exits without reading
its stdin closes the pipe, and a synchronous `write_all` of the payload then
fails with `EPIPE` before the runner has even started waiting. Writing on a
thread and ignoring its result is what makes "a hook need not read stdin"
true.
