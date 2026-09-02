---
name: thurbox-cli
description: The thurbox-cli binary: every subcommand group (agent, session, automation, task, message, editor, config, extension, version, update, notify, perf, plugin), soft vs force delete and restore, session lifecycle hooks (hooks.toml), parent lead/worker sessions, manual session ordering, the inter-session message mailbox, Exec automations and the heartbeat keeper, plus tasks/todos. Use when changing or driving thurbox headlessly, or working on any of those subsystems.
---

# thurbox-cli, automations, tasks and messages

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## thurbox-cli

A second binary (`thurbox-cli`) drives the same SQLite-backed,
tmux-hosted sessions headlessly (no TUI). It shares the database
with the TUI; changes appear via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-cli
thurbox-cli session create --name demo --repo-path /path \
    --agent codex --worktree-branch feat/x
# Spawn on a remote host from hosts.toml (worktree + tmux live remotely):
thurbox-cli session create --name demo --repo-path /srv/repo \
    --host devbox --worktree-branch feat/x
# Spawn a worker under a lead session (parent must exist):
thurbox-cli session create --name worker --repo-path /path \
    --parent <lead-uuid>
# Multi-repo: each --add-repo gets its own worktree on --worktree-branch;
# --add-dir attaches a repo as-is (no branch). The agent launches in a
# symlink workspace gathering every repo. Works on `task create` too.
thurbox-cli session create --name demo --repo-path /a \
    --agent claude --worktree-branch feat/x \
    --add-repo /b@main --add-repo /c@master --add-dir /reference
thurbox-cli session list                       # human-readable table
thurbox-cli session list --json | jq           # machine output for scripts
thurbox-cli session list --parent <lead-uuid> --json | jq  # direct children only
```

Subcommands: `agent` (launch-args — see below), `session` (create/list [`--deleted`]/get/delete/restore/restart
[`--if-missing`]/stop/start/fork/exec/meta/send [`--no-enter`]/key/capture/focus/signal/doctor/sync/register —
`sync`/`register` and the flags serve session sharing, ADR-24), `watch` (stream
session changes as newline-delimited JSON), `runtime` (status/stop — what
thurbox runs that is not a session), `automation` (alias `auto`:
create/list/show/edit/remove/run/runs/tick), `task` (alias `todo`:
create/list/show/edit/remove/run), `message` (alias `msg`:
send/inbox/prune — the inter-session mailbox queue; see below), `editor`
(get/set the Ctrl+O editor command; `editor mode <auto|terminal|gui>` chooses
how it launches — terminal editors get a real TTY via a tmux popup or TUI
suspend, GUI editors spawn detached; see the Editor Integration section of
`docs/FEATURES.md`), `config`
(validate/show — strict-parses every config file / prints the
effective resolved config; see `docs/CONFIG.md`), `extension`
(alias `ext`: install/uninstall/reinstall/list/available/update/activate/
deactivate/status — manage opt-in extensions; see below), `version`
(prints the running version; `--check` queries GitHub's latest release —
gated on `[features] version_check`, on by default for 1.0), `update`
(downloads, verifies, and replaces the installed binaries with the latest
release **within the current major** — a new major is reported, never installed,
because 2.x replaced the whole interface; `--force` bypasses the
up-to-date/dev-build/major guards; gated on
`[features] auto_update`, on by default for 1.0; the TUI also runs this silently on
startup when the flag is on), `notify`
(diagnose OS desktop notifications: prints the detected delivery backend
and last error; `--test` fires a sample — see OS notifications below), `perf`
(print the perf snapshot a running TUI publishes while `THURBOX_PERF_LOG`
or its perf HUD is active — see `docs/PERFORMANCE.md`), `plugin`
(v2 interface plugins without a TTY: `dir` reports the directory in force and
which of the two rules chose it, `new <name>` writes a starter that already
loads, `check` loads the interface the way `thurbox` does and exits non-zero on
a failure — **including on a pane that loaded but which no arrangement places**,
printing the `layout.lua` line to add — `list` is the same inventory the settings
modal's Interface tab shows, and
`events` lists every event a plugin may subscribe to with its payload, and
`install|sync|update|remove|available` manage panes from a declarative spec
— see `docs/PLUGINS.md`).

### A session reference is a name, a UUID, or an id prefix

Every session verb takes the same reference, resolved in that order: a full
UUID first (unambiguous by construction), then an exact name, then a unique id
prefix. **Ambiguity is refused, never guessed** — names are not unique (thurbox
does not enforce it, and a mirrored host contributes rows that legitimately
collide), so a reference matching two sessions exits non-zero and names both
ids. `--parent` resolves the same way.

`session create --on-existing <allow|adopt|replace|fail>` answers "a session of
this name already exists" — one question, four answers, because none of them is
safe to assume:

| Mode | Behaviour |
|---|---|
| `allow` (default) | create another one; both are then addressable only by id |
| `adopt` | return the existing session with `created: false` — idempotent, what a driver reconciling desired state wants |
| `replace` | tear the old one down (`delete --force`) first |
| `fail` | refuse, naming the id in the way; exit 1 |

`allow` is the default because thurbox **cannot** enforce uniqueness: a database
mirroring a shareable host (ADR-24) holds that host's rows beside its own, and
two machines may each legitimately have a session called `build`. Uniqueness is
something a caller asks for per creation, not a property of the namespace — which
is why `fail` exists at all, and why both firstmate and a Gas City provider were
each hand-rolling it with their own list-then-create race.

`adopt` and `replace` refuse an *ambiguous* name (one matching several
sessions), for the same reason the reference resolver does: adopting one of two,
or destroying one of two, is a guess. Every mode is decided before anything is
spawned, so a refusal leaves no window, worktree or row behind. It is a check,
not a lock — two simultaneous creates can still both pass, which is inherent to
a spawn that must make a multiplexer window before it has a row.

### Any command can be a session

`--agent` names an `agents.toml` entry; `--command` **is** the definition:

```bash
# A shell — the ready-made form, and a built-in agent
thurbox-cli session create --name probe --repo-path . --agent shell

# Anything at all, with its own environment
thurbox-cli session create --name build --repo-path . \
    --command npm --arg run --arg watch --env NODE_ENV=development
```

A `--command` session persists its **launch recipe** (command, args) on its row,
because there is no registry entry to re-resolve; `session restart` replays it
verbatim. A registry agent deliberately stores no recipe, so it is resolved by
name at every launch and editing `agents.toml` then restarting still takes
effect. `--env` is the exception both kinds store: it is the *caller's*, not
the registry's, so a registry agent's row carries it too — replayed on restart
and reproduced by `session exec`.

What a command session does **not** have is a conversation. `resume_args` and
`fork_args` are what address one, and only an agent definition declares them —
so `--resume` is refused for a raw command (the error names the fix: give it an
`[[agents]]` entry), and `session fork` gives you a second session in the same
directory rather than a continued one. Thurbox never learns what a conversation
*is*; it only knows how to address one.

### Parking a session: `stop` / `start`

`session stop` kills the pane and keeps everything else — the row, the checkout,
the branch, the agent's own history on disk. It is the verb between "leave it
running" and "delete it": reclaiming a heavy agent's pane used to mean deleting
the session, which also removed its worktrees.

A stopped session is marked, not merely pane-less, because three things repair a
session that has no pane on sight — the interface's respawn of surveyed rows, a
peer's `restart --if-missing` after a reboot, and extension self-heal. All three
skip a stopped row; `session start` is the only caller that clears the mark.

The mark is **reported by the read verbs**, not only by `watch`: a parked
session stays in `session list` and carries `stopped: true` with
`state: "stopped"` on `get` and `list` — the same key and type the stream uses.
`state` is `stopped` rather than the agent's latched last word or one of the two
silences, because all three describe a session that is running. `backend_id`
keeps naming the window it had (that is what the row records; `session start`
replaces it), and `send`/`key`/`capture` refuse a parked session by name instead
of reaching for the window that is gone.

### `session exec` — run something in a session's context

```bash
thurbox-cli session exec worker -- git status --porcelain
```

A separate process in the session's directory, on the machine the session lives
on, **under the session's own environment** — its recorded `--env` plus the
`THURBOX_*` identity its pane carries, with the *caller's* `THURBOX_*` scrubbed
so a driver running inside one session cannot lend the child that session's
identity (a `session signal` through `exec` used to record for the caller, with
exit 0). The environment used is in the result's `env`. Deliberately **not**
typed into the pane, which belongs to the agent and would interleave with
whatever it is doing. The command's exit code is always in the output;
`--exit-passthrough` additionally makes it this invocation's own — a command
exiting 2 is *that command's* 2, not a usage error, which is the distinction
Gas City's `proc.exec` capability is defined by. `exit_code` is `null` when the
command was terminated by a signal rather than exiting; passthrough then takes
the generic failure code, since there is no code to carry.

Arguments to `--command` may start with a dash (`--arg -c`): passing a switch is
the usual reason to pass an argument at all.

### `agent launch-args` — the hook wiring, for a driver that launches its own agent

```bash
thurbox-cli agent launch-args claude                      # command + args + env
thurbox-cli agent launch-args claude --session <ref>      # …resolved for one session
```

Status hooks are **arguments**: the `hooks` extension installs them by appending
to an agent's `args` in `agents.toml` (`--settings <hooks>.json` for claude), so
they reach the process only when thurbox builds the command line. A driver that
launches the agent itself — `session create --command`, or typing into a shell
session — therefore got no hooks, so `state` never populated and `watch` never
mentioned that session. This prints what thurbox would run; pass the args
through and the hooks are there.

`--session <ref>` resolves it for one session: the conversation id is pinned to
that row's, the host adapts the args (a remote session's hook configs are
shipped there), and the env carries the `THURBOX_SESSION` the agent's
`session signal` will report under. Without it the env names only this instance
(config dir, data dir, socket). Always a **fresh** launch — continuing a
conversation is `session start`/`restart`.

### `session meta` — the driver's key/value space

`set`/`get`/`list`/`unset`, namespaced by convention (`fm.*`, `gc.*`), never
interpreted by thurbox. Without it a driver's identity ends up encoded in the
session *name*, which then has to be parsed and kept unique and inside the
64-character limit. `set` reads the value from stdin when it is not an
argument.

`get` answers with the **bare value** in every format but JSON — being
captured into a shell variable is what makes stdout a pipe, so the piped
default would otherwise hand back the record in exactly the case the command
exists for. An unset key produces nothing; `--json` returns the record, and is
the only form that tells a `null` value from a key that was never set.

### `thurbox-cli watch` — nothing has to poll

```bash
thurbox-cli watch --initial | while read -r line; do …; done
```

One JSON object per line as sessions appear, change state, or go —
`{"event":"changed","session":"…","name":"…","state":"working",…}`. The
mechanism is the `PRAGMA data_version` gate the sync worker already uses, so it
works with no interface running and costs a pragma per tick rather than a query.
`--session` narrows it to one, `--for-secs` bounds it, `--initial` emits the
current state first so a starting driver gets its baseline and every change
after it in one stream.

### Remote sessions are driven, not refused

`send`, `key`, `capture` and `exec` work on a `--host` session: they delegate to
that host's own `thurbox-cli` (the mechanism the mirror pass already uses).
A refusal survives only where delegation is genuinely impossible — no
`hosts.toml` entry, or no reachable CLI there — and says which.

### `runtime` — what thurbox runs that is not a session

The automation heartbeat keeper is a detached tmux window created implicitly by
anything that arms an automation. It is not a session, so no session listing
showed it and no delete reclaimed it. `runtime status` reports it and the socket
in force; `runtime stop` kills it (the next `automation` write arms it again).

### thurbox-cli is an AXI

`thurbox-cli` is shaped for the agent that runs it, not the person who
occasionally does — it follows **AXI** (`axi/1.0-2026-07`, <https://axi.md>),
the agent-ergonomics spec, and `axi-axi validate` scores it 10 pass / 0 fail.
The shape that follows from that:

- **Output is human-readable in a terminal and TOON down a pipe.** It used to
  be JSON down a pipe. TOON (`src/cli/toon.rs`, a conforming v4.1 encoder —
  <https://github.com/toon-format/spec>) declares each list's length and field
  names once instead of repeating every key on every row, which is about 40%
  fewer tokens on the same answer and 80% on `session list`, where the record
  is wide and the useful part is narrow. Force a format with `--json`
  (compact), `--pretty` (indented), `--toon`, or `--text`.
- **`--json` is unchanged** — every field, exactly the bytes it always
  produced. It is the format scripts parse, and every in-repo consumer (each
  extension's `*-snapshot.sh`, `flow-summary.sh`, `link-sessions.sh` and the
  `dispatch-*` scripts) already passes it explicitly — the snapshot scripts
  with a bats file beside them pinning it. A pipeline that relied on the
  *auto* JSON has to spell the flag out.
- **A bare `thurbox-cli` prints live state**, not a usage dump: every session
  with the `state` its hooks last reported — the same word and the same key
  `session list` publishes — the calling session's unread mail,
  and the counts that would otherwise take three more invocations
  (`src/cli/home.rs`). Exit 0.
- **List views default to three or four fields**, the ones that let an agent
  decide what to look at next; `--fields <list>|all` asks for others and
  `--json` gives the whole record. Free text is capped in the TOON view only,
  with the total and `--full` named in place — never in `--json`, which is
  what `session capture … --json | jq -r .output` needs.
- **A zero-result answer says so and names what it searched**, rather than
  printing `[]` — which an agent cannot tell apart from a command that failed
  quietly.
- **Errors are structured on stdout, never stderr**, and the exit code says
  which kind: `0` success, `1` the command ran and failed, `2` the invocation
  was wrong. The trap that follows is worth naming to integrators:
  `thurbox-cli … --json | jq -r .field` exits **0 with empty output** on a
  failure, because `jq` parsed the error object and the pipeline carries `jq`'s
  status (`pipefail` does not help — `jq` succeeded). Capture, branch on the
  status, then parse. Each carries a `suggestion` and a runnable `help[]` line. stdout
  carries **exactly one** document, so a command that renders its report and
  *then* asks for a non-zero exit (`session doctor` on a broken session,
  `config validate` on an invalid file) comes back as `cli::Outcome::Failed`:
  the report is the answer, the exit code is the verdict, and the sentence
  explaining it goes to stderr rather than becoming a second document `jq`
  cannot parse.
- Results can carry a `help[N]:` block of next steps. It is the one part of
  the output that is AXI convention rather than strict TOON (bare indented
  lines rather than the hyphen-space list items §9.4 asks for);
  `output::render_toon` says why.

The renderer is `src/cli/output.rs` — `CommandOutput` carries the JSON, the
human string, and an `AgentView` (label, fields, help, empty-state, text cap)
that the TOON rendering reads. A command that declares no `AgentView` still
renders as TOON; declaring one is worth it on the commands agents run in a
loop. `tests/toon_conformance.rs` pins the encoder against the reference
implementation on the spec's own 179-case suite.

**Typing into a session: `send` and `key`.** `session send <uuid> <text>` types
text and presses Enter; **`--no-enter`** types it and stops, leaving it
unsubmitted in the agent's composer — an integration that verifies what it typed
before submitting cannot use the submitting form, because that fires every steer
the instant it is typed. **`session key <uuid> <name>`** is the other half: one
named special key (`enter`, `escape`, `tab`, `backspace`, `space`, the arrows,
`home`/`end`, `page-up`/`page-down`, `delete`, or `ctrl-<letter>`), spelled
case-insensitively with either separator (`ctrl-c` = `ctrl+c` = `C-c`) and
resolved through the closed table in `agent::tmux::NAMED_KEYS`. The table is
closed on purpose: tmux does **not** validate a key name — an unrecognized one
is typed into the pane as literal text — so `session key` refuses what it does
not know rather than injecting `Escpe` into somebody's prompt. Text goes out
bracketed-paste-wrapped either way (`paste_prompt_args`), which is what makes it
literal: no shell sees it, a leading `-` cannot read as a `send-keys` flag, and a
newline cannot submit the line before it. The one-shot helpers themselves drive
only this machine's tmux server, so `send`/`key`/`capture` on an `ssh:`/`wsl:`
backend are delegated to that host's own `thurbox-cli` (`delegate_to_host` in
`src/cli/sessions.rs`) instead of failing as a tmux status code against a
window that was never there. The refusal survives only where delegation is
genuinely impossible: a backend with no `hosts.toml` entry, or one whose
`thurbox-cli` could not be reached.

`session delete <uuid>` **soft-deletes** by default — only the DB row is marked
deleted (the TUI tears down the tmux window/worktree on its next sync), and
`session restore` revives it. `--force`
(`session_ops::delete_session_headless`) also kills the tmux window, removes
the worktrees **thurbox created** + the symlink workspace, disables `send`
automations targeting the session, and clears its `session meta` key/value space
(the row is unrestorable, so the meta would otherwise outlive it) — for headless
cleanup with no TUI running. Teardown is best-effort
(failures land in the JSON report); the row is always soft-deleted last. A
worktree the session merely **opened** (`created_by_thurbox = 0`, schema v42) is
left on disk and listed in the report's `kept_worktrees`: `git worktree remove
--force` would take the uncommitted work in it too, which is thurbox's to discard
only for a directory it made.

A `--force` delete stamps `sessions.force_deleted` (schema v37): the row still
appears in the restore list **tagged `force-deleted`** and is restorable
**best-effort** — force-delete removes the worktree *directory* but not the git
branch, so restore reattaches each surviving branch's committed work
(`App::recreate_worktrees`); only uncommitted/untracked changes are gone. Because
that recovery is lossy, the headless `session restore` **refuses a force-deleted
row unless `--best-effort`** (its JSON then carries `best_effort: true`) — but
only when the teardown could actually have lost something. A session whose
worktrees were every one of them opened is restored without the flag, since
nothing was removed. A row with *no* worktrees stays refused: that is every row
predating the column, and the conservative reading is the one that cannot lose
work by being wrong.

`session_ops::restore::restore_refusal` is the single decision behind that, and
it answers two questions, not one: *could the teardown have destroyed anything*
(the lossy case above) and *can this restore deliver what it promises*. The
second refuses — force-deleted or not — a session holding a **borrowed worktree
that is no longer on disk**, naming the path rather than talking about
uncommitted work that was never touched: `restore_session` reinstates the stored
`cwd` untouched and `respawn` anchors on it, so the pane would open at a
directory that is not there. That second question is asked only of a **local**
session: a remote one's checkout lives on its host, so stat'ing the path here
answers about the wrong filesystem, and the host's own `session restore` — which
`restore_session_headless` delegates to — asks it again where the path actually
is. `--best-effort` says yes to either. The command line calls the same function
and only appends the `--best-effort` sentence, so it and the TUI cannot disagree
about what is restorable.

Restore still skips a worktree it cannot bring back — branch gone for one
thurbox cut, directory gone for one it borrowed — rather than failing outright;
that skip keeps `worktrees_recovered` honest and nothing more, since its result
is never written back to the row.
`restore_session` clears both `deleted_at` and `force_deleted`.

The **TUI** `Ctrl+D` soft-deletes too (with a `Ctrl+Z` undo window). The
`[features] soft_delete` flag (default `true`) governs only this TUI path: set it
`false` and `Ctrl+D` becomes a hard delete — the same
`delete_session_headless(.., force=true)` teardown — since there is no `Ctrl+Z`
for it. That hard delete is **conditional**: a confirmation appears **only when
the session has work at risk** — uncommitted/untracked files, unpushed commits, a
multi-worktree session whose other checkouts the snapshot does not stat, or a
state that can't be read at all (remote host / git error → confirm to be safe) —
itemizing what would be lost; a known-clean session is deleted with no prompt.
The assessment is the pane's (`at_risk` in `ui/plugins/10_sessions.lua`, reading
the snapshot's `git` stats — v1 computed it in Rust over `git::worktree_stats`),
and the question travels through the shared `store.confirm` to the confirmation
float (`ui/plugins/60_confirm.lua`) rather than a bespoke modal. `Ctrl+U` lists the deleted rows (`ui/plugins/80_restore.lua`,
a float) and `Enter` restores the one under the cursor; a row the kernel **would
refuse** asks first — through the shared `store.confirm` question, not a bespoke
modal — and only then issues `restore` with `best_effort`. What it asks about is
the snapshot's `restore_refusal` (`DeletedRow`, published per row and nil when
the restore would simply run), i.e. `restore_refusal`'s own sentence, not the
`force-deleted` tag beside it: the two differ in both directions, and the pane
must not describe a refusal it does not decide. The tag still says how the row
was deleted, and drives the muted styling. The flag never changes
`thurbox-cli session delete`, which stays soft unless `--force`.

### Session lifecycle hooks (`hooks.toml`)

The user's own commands, run **by thurbox** before and after it creates,
deletes, restarts or restores a session — eight events, `session.{pre,post}_
{create,delete,restart,restore}`, declared as `[[hooks]] { event, command,
timeout_secs }` in `~/.config/thurbox/hooks.toml` (seeded commented-out; read
at fire time, no cache, no restart). **Not** the `hooks` extension: that
installs status hooks *into* the agent CLIs (`<config>/hooks/`,
`session_ops::builtin_hooks`) — the code says `lifecycle_hooks`/`HookEvent`
for this one so the two never blur.

They fire once per operation for every caller because every caller already
ends in the same four pipelines: `session_ops::spawn_session_headless`
(the TUI's create *and* fork, the CLI, `spawn` automations, extension
self-heal), `delete_session_headless` (`Ctrl+D` soft and hard, the CLI,
extension uninstall), `restart_session_headless`, `restore_session_headless`
(the TUI's undo and — since this change — the CLI's `session restore`, which
used to clear the flag alone). `fire_pre` runs before the pipeline's first
side effect and a failure (non-zero exit, timeout, cannot start) is its
`Err`; `fire_post` runs after its last and returns the failures, carried as
`hook_failures` on `SpawnResult`/`ForceDeleteReport`/`RestartReport`/
`RestoreReport` and in the CLI JSON. `SpawnPhase::Hooks` is reported while
the pre-create hooks run, so the placeholder row says so.

- **Data**: `session::hook_def` — `HookEvent` (closed enum, serde-spelled
  as the dotted names), `LifecycleHook`, `HooksFile`, and `HookContext` with
  `env()` (the `THURBOX_*` set — unset, never empty, for an unknown fact),
  `json()` (the stdin document) and `workdir()`.
- **File**: `agent::hooks_config` mirrors `host_config` — seed, strict
  parse through `parse_toml_reporting_unknown`, empty-with-warning on
  failure; `hooks_for(event)` in file order. `config validate`/`show` cover
  it.
- **Runner**: `session_ops::lifecycle_hooks::run_hook` — `platform_shell`
  (shared with `Exec` automations), `ctx.env()` + `thurbox_env_overrides()`
  (shared with `inject_thurbox_env`, so a `thurbox-cli` inside hits the same
  DB and the same tmux socket), JSON on a piped stdin, both output pipes drained on threads, a
  `try_wait` poll against the timeout, `kill()` at the deadline. Synchronous
  by design — it runs on whichever thread runs the operation (a worker in
  the TUI, rule 5), and `session_ops` has no runtime to lean on.
- **Cwd rule**: the primary repository when it is a local directory, else
  thurbox's own — the one path that exists at `pre_create` (no worktree yet)
  and at `post_delete` (worktree gone). A remote session's hook runs
  locally with `THURBOX_HOST` set.
- Proof: `tests/create_e2e.rs` (the pairs fire once each with the facts, a
  hook's `thurbox-cli` finds the row, a veto leaves nothing behind and
  surfaces through the command bus, a post failure leaves the session
  running); unit tests beside each module. User docs: `docs/CONFIG.md` →
  hooks.toml.

### Parent sessions (lead/worker)

Sessions carry an optional **`parent_session_id`** so orchestration scripts can
model lead → worker relationships. `session create --parent <uuid>` sets it (the
parent must be an existing active session — validated before any side effects);
`session list`/`get` emit it in the JSON (`null` for top-level) and `session list
--parent <uuid>` filters to direct children. The link is **purely
informational**: deleting a parent never cascades (orphans render as top-level),
and the parent is only validated at creation. In the TUI, **`Ctrl+F` fork**
records the source session as the fork's parent; the session list nests children
under their parent **within the same repo group** (muted `└` tree prefix; a child
whose parent renders in another group keeps its own position with a `↳` mark).
The nesting lives in `ui/lib/session_model.lua` (`session_model.build`, which
walks the snapshot's rows into depths — a port of v1's `compute_session_order` —
memoized on the published table's identity for the pane in
`ui/plugins/10_sessions.lua`), so
`Ctrl+J`/`Ctrl+K` navigation follows the tree automatically. A `Parent:` row is
the out-of-tree
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) plugin's,
not the bundled interface's. Storage: nullable `sessions.parent_session_id`
(schema v30; v29 is reserved by an in-flight branch).

### Manual session ordering

The session list is **manually orderable**: `Shift+J`/`Shift+K` (session list
focused; rebindable `SessionListMoveDown`/`SessionListMoveUp`) move the selected
session one row down/up. Manual order **wins** — status changes only recolor the
dot, never move a row. A move swaps two adjacent *blocks* (a row plus its nested
children, so a parent drags its subtree): root rows swap within their repo group,
the **whole group** swaps past a group edge, and nested children move among their
siblings only. It is computed over the items the pane actually rendered
(`ui/lib/order.lua`'s `move_block`, `root_ranges` and `child_ranges` — ports of
v1's `move_in_order`), and the result is handed back whole as one
`Command::Order { list }`: the kernel densely renumbers all sessions `0..n` and
persists, so the order survives restarts and syncs across instances via
`data_version` polling. Storage: nullable `sessions.display_order` (schema v31);
`None` = never moved, renders after ordered sessions in creation order (new
sessions append to their group). **`Shift+S`** (rebindable
`SessionListSortAlphabetically`) sorts by name **within each repo group** in one
shot, preserving group order (still by lowest `display_order`) and parent/child
nesting, and issues the same `Order` command (v1's
`sort_alphabetically_within_groups`).

All of that is the **grouped** shape. With the pane's `group_by_repo` setting
off there are no group edges to swap past: `ui/lib/session_model.lua` builds one
flat group ordered by `display_order` alone, so `Shift+J`/`Shift+K` move a row
one place anywhere in the list and `Shift+S` sorts the whole of it. Off has to
mean ungrouped rather than merely unlabelled — suppressing the header line while
keeping the clustering made every cross-repo move persist and then be undone by
the next build's re-clustering, with the headers that would have explained it
turned off. Parent/child nesting is unaffected: it is not a repo property.

### Inter-session messages (mailbox queue)

A general, agent-neutral **message queue** lets one session hand another a
**structured payload** without scraping its rendered terminal — the channel
extensions use for agent↔agent coordination (flow's clarify→plan→build relay is
the first consumer). A message is addressed **to** a session and carries a
free-form `kind` tag (`questions`/`plan`/`result`/… are conventions, not an enum),
a `body`, and optional provenance. Storage is the `session_messages` table (schema
**v32**, CRUD in `storage/messages.rs`); `Database::claim_messages` is a single
`UPDATE … RETURNING`, so the TUI, a cron tick, and a wake nudge can drain
concurrently without double-processing.

- **Identity (the registry key, self-knowable).** A session's `SessionId` is
  **stable for life** — `respawn_stale_session` reuses the original id on
  re-adoption (no soft-delete + new-row churn), so a cached id or queued message
  never goes stale. At spawn thurbox injects `THURBOX_SESSION` (= the `SessionId`,
  threaded via `SessionConfig.session_id` so it's known *before* launch and reused
  on respawn) and, for task-spawned sessions, `THURBOX_TASK` (= the task id) —
  both distinct from the older `THURBOX_SESSION_ID` (= `agent_session_id`, read by
  the metrics statusline). So a `thurbox-cli` call *inside* a session proves its
  own identity without scraping panes or names.
- **Consequence for the CLI surface**: an agent passes **no ids**. `message send
  --to <uuid|name>` stamps provenance from the injected identity, `message reply
  <message_id>` routes back to that message's sender (the replier never learns a
  peer's session id), and `message inbox [--claim]` defaults `--for` to the
  calling session. A send/reply with a wake also arms the automation heartbeat so
  a missed wake is still drained headless.

**Full flag list, the body/kind limits, backpressure cap, and retention/pruning
are in the Inter-Session Messages section of `docs/FEATURES.md`.**

An automation's `AutomationAction` is one of: **Send** (paste a prompt into a
running session), **Spawn** (start a fresh session and prompt it), or **Exec**
(run a shell command headlessly — `sh -c`, or `cmd /C` on Windows — with no
agent/session; its exit status + tail-truncated output land in the run history).
`Exec` is the deterministic-scheduled-job action (the task-integration sync
extensions use it). The runner is `session_ops::run_exec_command`, which blocks
until the child exits and is called from exactly one place —
`cli::automations`'s `tick`. Firing is **CLI-only in v2**: the interface neither
runs schedules nor holds a worker for them, so there is no in-flight/`skipped`
bookkeeping in the binary that draws the screen. The command is stored in the
`action_command` column (schema **v36**, on both `tasks` and `automations`).
Author one headlessly with `thurbox-cli automation create --command "<shell>"`
(mutually exclusive with `--session`/`--repo`), or from an extension manifest
(`[[automations]]` with a `command` field instead of `session_ref`/`prompt`).
`Task.action` shares the enum but tasks never carry an `Exec`
(it's automation-only).

Automations fire even when the TUI is closed: a tmux heartbeat
keeper window (`automation-heartbeat`, armed on TUI startup and on
`automation create`) loops `automation tick` every 60 s and keeps
the tmux server alive. `packaging/` ships opt-in systemd/launchd
units for reboot-proof firing. Concurrent firers are de-duplicated
by `Database::claim_due_automation` (atomic CAS), so the keeper,
an OS timer and a hand-run `tick` never double-fire.

**No automations pane.** The interface has none, and `[features] automations`
no longer hides one: its only *effect* in the TUI is gating **arming the tmux
heartbeat keeper** at startup (`src/main.rs`; it also rides the live-reload merge
as a restart-only flag and is published to Lua in `thurbox.features`). The rows
are published as `thurbox.automations` and the kernel accepts an `automation`
command (enable/disable/run/delete — `run_now` only marks it due, so the tick
stays the one execution path), so a pane is a plugin somebody can write: both
halves are done and nothing in `ui/` uses them yet.

> A pane is owed. v1's shape — a pane under the session list, sharing one
> circular `j`/`k` list with it, with the centre pane as its editor and the run
> history below — is on the `v1.x` branch; it is deliberately not described here
> as if it existed.

## Tasks (todo list)

Todo items (title + markdown description + status), **CLI-only**: the interface has
no tasks pane. The data, the storage and the agent linkage are unchanged, so
extensions and scripts that used them still work.

A task can be **acted on by a coding agent**: `Task::agent_prompt()` builds an
`id + # title + markdown description` block plus self-service hints (`thurbox-cli
task show <id>` to read the record, `thurbox-cli task edit <id> --status done` to
close it), and `task run` sends or spawns. Triggering advances `Todo → InProgress`.

- **Data** (`session/task.rs`): `Task` (`id`, `title`, `description:
  Option<String>`, `status: TaskStatus` {`Todo`/`InProgress`/`Done`}, `action:
  Option<AutomationAction>`, plus `source`/`external_id`/`external_url` for
  tracker sync — `source = "local"` for native todos, a tracker tag for imported
  ones. `(source, external_id)` is the dedup key.
- **Storage** (`storage/tasks.rs`, schema v25/v26): `tasks` mirroring the automation
  action columns plus a nullable `description`, soft-delete via `deleted_at`,
  audited under `EntityType::Task`, `idx_tasks_external` on `(source,
  external_id)` (v35) backing the upsert lookup.
- **CLI**: `thurbox-cli task` (alias `todo`) —
  `create`/`list`/`show`/`edit`/`remove`/`run`, with `--description` (markdown) and
  the external-sync flags. `[features] tasks` is **accepted and ignored** — nothing
  reads it, in the CLI or anywhere else.

> A pane is owed, and the shape a plugin would take is the same one
> `10_sessions.lua` uses: read the snapshot, return a tree, send a command.

