## Why

thurbox knows when a session is created, deleted, restarted and restored, but
the user cannot: there is no way to run their own command at those moments — copy
an `.env` into a fresh worktree, install dependencies before the agent boots,
post to a chat channel when a session lands, refuse a session on a branch that
must not be touched, clean a scratch directory when one is deleted. Today that
needs a wrapper around `thurbox-cli` (which the TUI never calls) or a fork.

The only "hooks" thurbox has run in the *opposite* direction — the built-in
`hooks` extension installs status hooks into the agent CLIs so they can tell
thurbox what they are doing. Nothing lets thurbox tell the user's own scripts
what *it* is doing.

## What Changes

- A new config file, `~/.config/thurbox/hooks.toml`, declares **session
  lifecycle hooks** as data: `[[hooks]]` entries pairing an event name with a
  shell command (and an optional timeout). Seeded commented-out on first run,
  like `hosts.toml`; absent or empty means no hooks, exactly today's behaviour.
- Eight events, pre and post for each of the four session operations:
  `session.pre_create` / `session.post_create`, `session.pre_delete` /
  `session.post_delete`, `session.pre_restart` / `session.post_restart`,
  `session.pre_restore` / `session.post_restore`.
- **`pre_*` hooks can veto**: a non-zero exit or a timeout aborts the operation
  before it has any side effect, and the hook's stderr is the reported reason.
  **`post_*` hooks are informational**: every one runs, a failure is logged and
  reported but never fails the operation.
- Hooks receive the session's facts as **`THURBOX_*` environment variables**
  (event, session id, name, agent, repository, working directory, branch, base,
  host, parent) and, for anything structured, the same facts plus the worktree
  list as **one JSON document on stdin**. A `thurbox-cli` call inside a hook hits
  the same database as the thurbox that fired it, the way an agent's status hook
  already does.
- Hooks fire **once per operation, for every caller**: the TUI, `thurbox-cli`,
  a `spawn` automation, an extension's self-heal — because they fire inside the
  one pipeline each operation already has, not in each interface.
- `thurbox-cli config validate` / `config show` learn the new file, and the
  creation progress a plugin renders gains a phase for the pre-hooks, so a slow
  hook is named rather than mistaken for a slow fetch.

No existing behaviour changes for a user with no `hooks.toml` entries.

## Capabilities

### New Capabilities

- `session-hooks`: which session lifecycle events exist, how a user declares a
  command for one, what the command receives, when it runs relative to the
  operation, and what its exit status means (veto vs. informational).

### Modified Capabilities

None. `session-creation`'s requirements hold as written — a vetoed creation is
"a failure reported through the in-flight channel, leaving no half-created
session", which it already requires — and `core-settings` covers
`settings.toml`, which this does not touch.

## Impact

- **New**: `session::hook_def` (pure data: events, entries, the context handed
  to a hook), `agent::hooks_config` (load-or-seed of `hooks.toml`, mirroring
  `host_config`), `session_ops::lifecycle_hooks` (the runner: env + stdin +
  timeout + output capture).
- **Touched**: `session_ops::{spawn,delete,restart,restore}` gain a pre and a
  post call each; `SpawnPhase` gains a `hooks` phase; `SpawnResult`,
  `ForceDeleteReport` and `RestoreReport` carry post-hook failures;
  `cli::config` validates and shows the file; the CLI's JSON surfaces the
  failures.
- **Not touched**: the kernel, the Lua interface, `settings.toml`, the
  database schema, the agent status-hook extension. `tests/architecture_rules.rs`
  needs entries for the new modules.
- **Docs**: `docs/CONFIG.md` (a `hooks.toml` section, and a sentence drawing the
  line between it and the `hooks/` extension home beside it), `docs/FEATURES.md`,
  `CLAUDE.md`.
- **Performance**: a pre-hook adds its own runtime to an operation, bounded by
  its timeout (default 30 s); it runs on the thread that already runs the
  operation — a worker in the TUI — so the render loop is never on the path.
