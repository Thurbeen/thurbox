# Built-in Agents Reference

The exact configuration and runtime behavior of every **built-in** coding
agent, plus the checklist for **adding a new one**.

Thurbox is agent-neutral: it knows nothing about a coding agent's model,
prompts, permissions, or tools — only how to launch the CLI and how that CLI
resumes, forks, and reports status. Every agent is **data**, not code. The
built-ins are just the entries thurbox ships pre-seeded; a user adds their own
by editing `agents.toml` (see [config.md](config.md#agentstoml) for the file
format). This document is the single source of truth for **what each built-in's
data actually is and why**, and it exists because adding a new built-in touches
several files that are *not* updated automatically — see
[Adding a new built-in agent](#adding-a-new-built-in-agent).

Related docs:

- [config.md → agents.toml](config.md#agentstoml) — the config-file format and
  per-field syntax (`{id}` substitution, `resume_latest`, `hook_schema`).
- [features.md → Agent definitions](features.md#agent-definitions) — the
  agents-as-data design.
- [architecture.md](architecture.md) — the `session::AgentDef` purity rule.
- `../.agents/skills/thurbox-agents/` → *Agent Definitions* — the in-repo
  working reference.

## Where the built-ins live

The built-in registry is **not** hand-constructed in Rust. It is an embedded
TOML document in [`src/agent/agent_config.rs`](../../crates/thurbox/src/agent/agent_config.rs):
`builtin_agents_toml()` joins `BUILTIN_AGENTS_HEAD` (the coding agents), the
`shell` entry spelled for this platform (`SHELL_AGENT_TOML`) and
`BUILTIN_AGENTS_TAIL` (the commented examples). `builtin_registry()` parses
that string, and `seed_agents_toml()` writes it verbatim to
`~/.config/thurbox/agents.toml` on first run. So the same text is both the
in-binary fallback and the seed the user then edits — keep it authoritative.

An `AgentDef` says *how to launch* an agent, and nothing about how it reports
state back: thurbox installs nothing into an agent's own config. See
[Status hooks](#status-hooks).

## The built-in agents

Ten entries ship pre-seeded — nine coding agents and a plain shell.
`default = "claude"`.

| Name | Command | Resume | Fork | ID model |
|------|---------|--------|------|----------|
| `claude` | `claude` | `--resume {id}` | `--resume {id} --fork-session` | **pinned** (`--session-id {id}`) |
| `codex` | `codex` | `resume {id}` | `fork {id}` | hook-reported ID |
| `antigravity` | `agy` | `--continue` | — (none) | id-less (`resume_latest`) |
| `opencode` | `opencode` | `--continue` | `--continue --fork` | id-less (`resume_latest`) |
| `aider` | `aider` | `--restore-chat-history` | — (none) | id-less (`resume_latest`) |
| `copilot` | `copilot` | `--continue` | — (none) | id-less (`resume_latest`) |
| `vibe` | `vibe` | — (starts fresh) | — (none) | none |
| `pi` | `pi` | `--session-id {id}` | `--fork {id}` | **pinned** (`--session-id {id}`) |
| `omp` | `omp` | `--session-dir {home}/.omp/…/thurbox-{id} --continue` | — (none) | **path-pinned** (`--session-dir <dir>`) |
| `shell` | `bash -i` (`powershell -NoLogo` on a Windows host) | — (none) | — (none) | none |

`claude`, `codex` and `vibe` also ship `telemetry_env` / `telemetry_args`,
applied only while `[telemetry]` is on; `copilot`, `omp` and `opencode` need
only the spec's `OTEL_EXPORTER_OTLP_*` variables, which every session gets, and
`antigravity`, `pi` and `aider` have no native OTel. See
[config.md](config.md#telemetry--opentelemetry-for-sessions).

### Several Claude accounts

Create and authenticate each account's configuration directory with Claude's own
CLI, then add one agent per directory to `agents.toml`:

```toml
[[agents]]
name = "claude-work"
command = "claude"
hook_schema = "claude"
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]
additional_dir_args = ["--add-dir", "{dir}"]
env = { CLAUDE_CONFIG_DIR = "{home}/.claude-work" }
```

Choose that agent in the TUI or pass `--agent claude-work` to `session create`.
The config directory is saved with the session, so restart and fork use the same
account even after the agent definition changes. `hook_schema = "claude"` reads
this custom name's status as Claude's; status and messaging follow the
session's configuration directory and identity.

Every conversation argument group may also contain `{name}`. Thurbox replaces it
with the display name captured for that launch as one literal argument. A rename
does not alter the running process; a later restart uses the persisted new name.
The same snapshot is available to every agent and direct command as
`THURBOX_SESSION_NAME`.

Repeat the entry for another account directory. On a shared host, define
the agent on that host. Tokens remain in the agent's own account
configuration, outside `agents.toml`.

A Codex account works the same way, with `CODEX_HOME` as the directory:

```toml
[[agents]]
name = "codex-work"
extends = "codex"
env = { CODEX_HOME = "{home}/.codex-work" }
```

### Multi-repo directory scope

The built-in Claude definition sets
`additional_dir_args = ["--add-dir", "{dir}"]`. For a multi-repo launch,
Thurbox repeats this group for each session member: the primary checkout,
additional worktrees, and attached directories. It passes checkout paths,
never their parent directories or the original repository of a worktree.
Other CLIs can declare their own group; an empty group emits no arguments.
`{dir}` is substituted as one argument, so spaces do not split a path.

The symlink workspace remains the process cwd. Claude checks both a requested
path and its resolved target; without explicit target scope, edits through a
workspace link can prompt on every file. This follows the security tightening
recorded in [Claude Code 2.1.69's changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md#2169)
and its [symlink permission rules](https://code.claude.com/docs/en/permissions#symlinks).
Additional directories follow the selected permission mode: `acceptEdits`
auto-accepts ordinary member edits, while Manual and Plan retain their own
rules. Deny/ask rules and protected paths still apply. Initial workspace trust
is separate and can still prompt.

Existing `claude` definitions using `command = "claude"` gain this group in
memory when the field is absent. An explicit `additional_dir_args = []` opts
out. Custom Claude definitions and wrappers must set the group explicitly;
`hook_schema` describes status hooks and does not imply CLI directory flags.
Thurbox does not rewrite global Claude settings or change the permission mode.
Raw `--command` recipes remain verbatim; callers supply their directory flags.
Additional scope grants file access, without turning every member into a full
Claude configuration root; member-local settings follow Claude's own discovery
rules. Non-UTF8 targets receive no scope arguments, because replacing their
bytes could grant access to another directory.

After upgrading, restart affected sessions to apply the scope arguments;
creation, restart/resume, fork, and session-context agent launch plans derive
scope from the persisted member set. Single-repo launches emit no extra scope.
Local POSIX member paths are canonicalized; native Windows and remote paths
are passed in host-native form for Claude to resolve there. Most UNC paths
cannot be added to Claude; use a mapped drive as its documentation prescribes.

### `shell` — the entry that is not a coding agent

`shell` exists so a session can be something other than an agent: you run
whatever you like in it, and an external driver can start its own tool with its
own flags rather than asking thurbox to model them. It is the ready-made form of
`session create --command`, and the only built-in whose `command` differs by
platform, because a native-Windows host runs psmux and PowerShell rather than
tmux and bash.

The platform is the **session's host**, not the one thurbox was built for. The
seed spells `shell` for the machine it is written on, and while the entry still
holds that stock `command` and `args`, a session on a host of the other platform
gets that host's shell instead — a Linux thurbox creating a session on a Windows
SSH host sends it `powershell -NoLogo`, not a `bash -i` that would start WSL
there. A `shell` whose `command` or `args` you changed is launched as written,
wherever the session runs.

`args = ["-i"]` on POSIX is load-bearing rather than decoration: a plain `bash`
in a pane executes what is sent to it but renders **no prompt and no echo**, so
anything reading the screen to decide whether the session is ready sees a blank
pane that is nonetheless working.

It declares no `resume_args` or `fork_args`, and that absence is the statement:
a shell has no conversation. `session restart` replays its recipe in the same
directory (a shell barely notices — its history and cwd live on disk),
`session fork` gives you a second shell beside it, and `session create --resume`
silently starts fresh rather than attaching to anything (only a raw
`--command` session refuses `--resume` outright; see
[features.md](features.md#a-launch-recipe-is-not-a-conversation)). Nothing
wires status hooks for it either, so a `shell` session reports `hook_state: null`
until something in it calls `thurbox-cli session signal` — which anything in
the pane can, since
`THURBOX_SESSION` is in its environment.

### ID model: pinned vs. id-less

This is the single most important behavioral distinction, because it decides
whether resume/fork can target *this* session or only *the last* one.

- **Pinned** (`claude`, `pi`) — the CLI accepts a thurbox-generated UUID at
  creation via `new_session_args = ["--session-id", "{id}"]`, so it can
  resume/fork **that exact session** by id. `resume_latest` stays `false`.
- **Path-pinned** (`omp`) — the CLI generates its *own* internal session id and
  can't accept thurbox's, but it takes a session **directory**. thurbox maps its
  UUID to a deterministic directory under OMP's default root
  (`--session-dir {home}/.omp/agent/sessions/thurbox-{id}` on create, the same
  plus `--continue` to restore), so restart reattaches the exact session even
  though the id itself is opaque. A session *file* does not work: OMP opens
  only a file that already holds entries (`Session "…" not found`, exit 1, as of
  18.10), so an older seed that passed `--session <file>.jsonl` killed every new
  omp session at launch; the loader swaps that exact legacy pair for the
  current one in memory, so an existing `agents.toml` needs no edit. A
  conversation an older OMP did write to a legacy `thurbox-{id}.jsonl` file is
  not reopened after that swap: the restart looks only in the directory. The new `{home}` token is expanded
  to the resolved home dir **at spawn time by thurbox** (args are POSIX-quoted, so
  a literal `~` would never expand), and it translates onto the remote/WSL home.
  It has no `fork_args` (OMP can't pin a fork's target file to a thurbox UUID), so
  `Ctrl+F` starts fresh.
- **Hook-reported** (`codex`) — `SessionStart` reports Codex's actual
  conversation UUID. Binding requires a pane-local `--no-daemon` Codex process
  with matching row and pane identity; shared-daemon hooks are not sufficient.
  Preserve the entry's sandbox, approval and trusted-hook settings when adding
  that launch flag. Thurbox saves the UUID beside the row's generated
  `agent_session_id` and resumes or forks that exact UUID. Existing rows with no
  captured UUID open Codex's interactive `resume` picker on restart; selecting a
  conversation binds it through the hook. Forking an unbound row opens Codex's
  interactive `fork` picker. Any `SessionStart` naming a different
  conversation — `startup`, `clear` or `resume` — is ambiguous, since a second
  Codex process inherits the same row identity and the pane's own `/new`
  reports `startup` too. It makes the next restart use the picker, and
  `message send` leaves the body in the mailbox rather than queueing it on the
  thread the pane may have left. Only the picker launch can bind its selection.
  Existing seeded `codex` definitions using `--last`
  are upgraded in memory; other custom definitions are unchanged. A remotely
  provisioned hook reports status through the pane option and has no channel to
  persist Codex's ID in the local database, so an unmapped remote row opens the
  picker again on restart.
- **id-less** (`antigravity`, `opencode`, `aider`, `copilot`) — the CLI
  can neither pin nor report a session id, so the agent's own flags resolve *"the
  last session in this directory"* (`--continue`,
  `--restore-chat-history`). These entries set `resume_latest = true` and use no
  `{id}` token. This works because restart reuses the session's cwd and a
  single-repo fork reuses the parent's cwd. **Caveat:** a *multi-repo* fork lands
  in a fresh symlink workspace, so `--last`/`--continue` finds no parent session
  there (multi-repo *restart* still works — same workspace dir).
- **none** (`vibe`) — no resume group at all, so it always starts fresh on
  restart. The live tmux process is what carries its state across TUI restarts.

`resume_latest` only changes **when** the resume group fires
(`session_ops::resume_trigger_for`): for id-less agents restart always triggers
resume; for `claude` it defers to an on-disk transcript check. Ahead of both,
an id-pinned session whose hook has reported a turn (`session signal` with
`THURBOX_SIGNAL_TURN=start`, which Claude's `UserPromptSubmit` hook sets) is
resumed: when the hook's stdin payload names the row's own id as its
`session_id`, the signal records it in `thurbox.conversation_started` session
meta. A fork's conversation carries an id Claude chose, so it records nothing
and keeps the transcript check. That is what keeps a custom-named Claude launcher resumable when
its wrapper script sets `CLAUDE_CONFIG_DIR` itself, so its transcript sits
where the transcript check cannot look and `--session-id` would be refused as
already in use.

### Fork

Agents that declare no `fork_args` (`antigravity`, `aider`, `copilot`, `vibe`,
`omp`) cannot fork — `Ctrl+F` starts a **fresh** session instead. None of those
CLIs support forking a conversation to a thurbox-pinned target.

### `claude`: messages held for approval

`message send` hands a body to a claude session over its inbox socket. Claude
Code can then hold it behind an approval dialog, notably in
`bypassPermissions`, unless the recipient's settings carry
`crossSessionInbound: accept`. The socket accepts the body either way, so the
send reports `delivered_via: "claude-socket"` with `may_hold: true`, and the
text output says the message may wait on a prompt. `may_hold` describes the
route: thurbox cannot see the recipient's permission mode or whether a dialog
was shown. A delivered row is marked read, so it shows under
`message inbox --all`, not plain `message inbox`.

Thurbox does not set `crossSessionInbound` itself: accepting cross-session
prompts with no dialog lets any peer session drive the recipient, which is a
choice for the user to make. To make it, put the setting in your own claude
settings (`~/.claude/settings.json`, or the one under `CLAUDE_CONFIG_DIR`).
Claude merges the `--settings` its launch carries over those, so they still
apply inside thurbox sessions. They also apply outside thurbox.

### Status hooks

Thurbox installs nothing into an agent's own config. Fleet's harness profiles
([`crates/fleet-api/src/harness.rs`](../../crates/fleet-api/src/harness.rs))
pass a harness its status hooks at launch: `claude` gets one `--settings` with
its hooks and `codex` a `notify` command. Another agent reports no status.
Hook entries an earlier thurbox wrote into an agent's config are removed by
hand: see [removing thurbox's old hook entries](https://thurbox.thurbeen.eu/docs/extensions.html).

### `hook_schema` (custom rebrands only)

The built-ins never set `hook_schema`. It exists for a **user's** custom agent
that runs a built-in under a different name (e.g. a rebranded-claude CLI called
`fleet`): set `hook_schema = "claude"` and its status is read as claude's.
`hook_schema` says nothing about accounts: thurbox reads no agent credentials and asks no
provider for quotas, which is the job of a plugin such as `thurbox-info-panel`.
See
[config.md](config.md#agentstoml) and the `AgentDef.hook_schema` doc comment.

## Adding a new built-in agent

Adding support for a new CLI as a **user** needs only an `agents.toml` edit — no
recompile (that's the whole point of agents-as-data). But promoting a CLI to a
shipped **built-in** touches several places that are **not** kept in sync
automatically. Work the checklist top to bottom:

1. **Seed entry** — add an `[[agents]]` block to `BUILTIN_AGENTS_HEAD` in
   [`src/agent/agent_config.rs`](../../crates/thurbox/src/agent/agent_config.rs). Set `command`,
   the resume/fork/new-session groups, and `resume_latest` per the ID model
   above. Add a short comment explaining the CLI's resume/fork semantics, as the
   existing entries do. **Decide the ID model first** — whether the CLI can pin a
   session id — because it determines the conversation argument groups.

2. **Status hooks** — thurbox wires none. An agent reports
   working/blocked/done only when its Fleet harness profile gives it hooks; see
   [Status hooks](#status-hooks).

3. **Docs — update every list of built-ins** (these are prose, not generated, so
   they drift):
   - [The built-in agents](#the-built-in-agents) table above.
   - [config.md → agents.toml](config.md#agentstoml) — the parenthetical list and
     the "N built-ins" count.
   - [features.md → Agent definitions](features.md#agent-definitions) — the prose
     list and the pinned-id sentence.
   - `../.agents/skills/thurbox-agents/` → *Agent Definitions* and
     *Custom-agent status hooks* — the built-in lists and the resume/fork
     caveats.

4. **Tests** — the seed TOML must still parse and produce the expected registry.
   Run the agent-config tests and update any fixture that enumerates the built-in
   set:

   ```bash
   cargo nextest run -E 'test(agent)'
   ```

5. **Verify end-to-end** (optional but recommended) — install the CLI and confirm
   in a sandbox that resume, fork, and the status dot all behave:

   ```bash
   scripts/dev/sandbox.sh
   ```

### What is auto-handled (don't do these)

- No provider code — `agent::GenericProvider` drives *any* `AgentDef` from its
  data; there is no per-agent Rust launcher to write.
- No agent picker change — the new-session picker reads the registry.
- No migration for existing users — `agents.toml` is only seeded when absent, so
  a new built-in appears for **fresh installs**; existing users keep their file
  and add the entry themselves (this is intentional — thurbox never rewrites a
  user's edited config).

## Local preparation before an agent starts

Two places can prepare a directory before an agent starts, and they answer
different questions:

- **`setup` in the agent's `agents.toml` entry** is part of *how this agent
  launches*: shell steps run in the agent's own pane, in its launch directory
  and environment, on every launch (create, restart, start, restore, fork) and
  on the host the agent runs on. A failing step aborts the launch and the
  session reads `failed`. See [config.md](config.md#templates-argument-order-and-setup).
- **A `session.pre_launch` hook in the user's `hooks.toml`** is *policy around
  launches*: one command for whichever agents it names, run by thurbox outside
  the pane, on create/fork only, on the machine the agent runs on (a remote
  session's host), before the window exists; a refusal leaves the prepared
  directories for inspection.

```toml
[[hooks]]
event = "session.pre_launch"
agents = ["claude"]
command = 'my-trust-script "$THURBOX_CWD"'
timeout_secs = 10
```

Either way the script is yours: Thurbox supplies the final cwd and worktree
paths without knowing an agent's trust-file format. See
[config.md](config.md#hookstoml) for the hook's contract.

### Replacing a launch wrapper

A wrapper script as an entry's `command` used to be the only way to run
something before the CLI, or to put a flag ahead of the session-selection
group, because thurbox emitted that group *before* `args`. The wrapper this
section replaces was symlinked once per role (`thurbox-role-coder`,
`thurbox-role-codex-coder`, …) and derived the role from its own name; it ran
`node scripts/worktree-setup.mjs` and `scripts/role.sh <role> --agent <agent>`
when they existed, then exec'd claude or codex with the role's flags. With
`vars`, `setup` and `{session_args}` the same entries are data:

```toml
[[agents]]
name = "claude-coder"
hook_schema = "claude"
command = "claude"
vars = { role = "coder" }
setup = [
  "[ -f scripts/worktree-setup.mjs ] && node scripts/worktree-setup.mjs",
  "[ -x scripts/role.sh ] && scripts/role.sh {role} --agent claude",
]
args = [
  "--permission-mode", "auto",
  "--model", "opus",
  "--add-dir", "{home}/dev/skills/thurbox/",
  "{session_args}",
]
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]

# Another role is a variant naming only what differs. `vars` merge per key.
[[agents]]
name = "claude-operator"
extends = "claude-coder"
vars = { role = "operator" }

# `ops` runs without --permission-mode, so it replaces `args`.
[[agents]]
name = "claude-ops"
extends = "claude-coder"
vars = { role = "ops" }
args = ["--model", "opus", "--add-dir", "{home}/dev/skills/thurbox/", "{session_args}"]

[[agents]]
name = "codex-coder"
hook_schema = "codex"
command = "codex"
vars = { role = "coder" }
setup = [
  "[ -f scripts/worktree-setup.mjs ] && node scripts/worktree-setup.mjs",
  "[ -x scripts/role.sh ] && scripts/role.sh {role} --agent codex",
]
# Codex takes its flags after `resume`/`fork`, never before them.
args = [
  "{session_args}",
  "--approve-for-me",
  "-p", "spotpay-{role}",
  "-c", "sandbox_workspace_write.network_access=true",
  "--add-dir", "{home}/dev/skills/thurbox/",
]
resume_args = ["resume", "--last"]
fork_args = ["fork", "--last"]
resume_latest = true
```

These launch as `claude --permission-mode auto --model opus --add-dir …
--session-id <id>` and as `codex resume --last
--approve-for-me -p spotpay-coder -c … --add-dir …` (or the same flags with no
subcommand on a fresh launch). A setup step that
fails — `scripts/role.sh` refusing an unknown role — stops the launch with the
session `failed` instead of starting an agent without its role.

What the entries cannot say, and why:

- **Arguments that depend on a file.** The wrapper passed `-p spotpay-<role>`
  only when `scripts/role.sh` had written that profile, so it worked in any
  checkout. An `args` token is always passed: the entry above is for checkouts
  that have `scripts/role.sh`; elsewhere codex is given a profile that does not
  exist and the launch fails. Use a plain `codex` entry for other repositories.
- **Codex's per-launch trust override.** The wrapper passed `-c
  projects={"<cwd>"={trust_level="trusted"},"<repo root>"={…}}`, computed at
  launch from `pwd -P` and `git rev-parse --git-common-dir`, so codex would not
  open on its "trust this directory?" prompt and swallow the brief. Those paths
  are only known once the session's directory exists, and no placeholder
  carries them; a `setup` step runs in that directory but cannot add
  arguments. Until codex can be told otherwise, keep that one step in a
  codex-specific script (a `command` that adds the `-c` and `exec`s codex), or
  record trust for each repository root in `~/.codex/config.toml` (the wrapper
  notes codex resolves a worktree's trust to its repository root; unlike `-c`,
  a config file's quoted keys handle the dots in worktree paths).
- **Diagnostics about an old `scripts/role.sh`.** The wrapper's warning when a
  checkout's `role.sh` predates `--agent` is not reproduced; a `setup` step can
  print its own.

`last_output_at` and `transcript_path` are read-side supervision fields, null
for a session on a remote host. The latter is an existing local Claude-family transcript under the session's
account directory, or null for other families/remote sessions. Neither is a
completion signal.

### Background sub-work

`background` means the turn is at rest and work it started is still running
or scheduled. Any agent's hooks may report it with `thurbox-cli session signal
--state background`. Two sources claim it.

**The agent's own account (claude).** claude's `Stop` hook passes its payload
on (`--state done --stop-payload -`), which carries `background_tasks[]` —
every task in flight: shells, subagents, Monitors, workflows, teammates, cloud
sessions, MCP tasks — and `session_crons[]`, the CronCreate / ScheduleWakeup /
`/loop` wake-ups. Either list non-empty stores `background`; both present and
empty stores `done` and proves the session idle; a missing list (a Claude Code
that predates it) is unknown and leaves the decision to the edges below.
Shape read from Claude Code 2.1.318's payload builder; the fixtures are in
[`tests/fixtures/claude_stop/`](../../crates/thurbox/tests/fixtures/claude_stop/). This is what
makes claude's `state_support` `full` — see
[Activity](orchestration.md#activity). A finished task or a fired cron starts a
new turn whose own `Stop` clears it.

**Edges**, where both edges of the sub-work reach the parent session:

| Agent | Start edge | End edge | Maps to |
|---|---|---|---|
| `claude` | `SubagentStart` with a non-empty `agent_type` | `SubagentStop`, same `agent_id` | `--work-start <id>` / `--work-end <id>` |
| every other built-in | — | — | nothing: reports exactly as before |

The other built-ins are `partial`: nothing they report says what a turn left
running, so idleness rests on a claim. codex's `SessionStart` hook prints an
instruction (folded into the model's context) to end a turn that leaves
nothing running with the line `[thurbox-idle]`, and its `Stop` hook hands over
the payload whose `last_assistant_message` carries it — verified against
codex-cli 0.154.0's hook schema, whose `Stop` input has that field. The rest
have no instruction channel thurbox could verify on the versions it was
written against (none was installed): opencode, pi and omp would need their
plugin APIs' system-prompt hooks, antigravity, grok and kimi a `SessionStart`
whose stdout reaches the model and a `Stop` payload carrying the final
message, copilot (whose `sessionStart` fires after the first prompt) and vibe
(no session event) another channel altogether, and aider has no turn end to
attach a claim to.

Evidence: the issue's reproduction on Claude Code 2.1.303 (headless, three
runs) and a headless run on 2.1.308, both recording `SubagentStart` → `Stop`
→ `SubagentStop` → `Stop` in the parent session for a background Agent-tool
subagent, with one `agent_id` and `agent_type: general-purpose`. Interactive
mode was not checked. A `done` while any counted work is running is stored as
`background`; the end of the last one stores `done`, which `watch` streams and
`session wait --state done` returns on. A typed subagent's own tool calls carry
its id (`THURBOX_SIGNAL_WORK`) and keep a resting turn in `background` rather
than starting a new one. A repeated edge counts once, and an end that arrives
before its own start cancels it.

Not mapped, by design:

- claude's own post-turn agent (an `agent_id` with no `agent_type`), whose end
  nothing waits on;
- `run_in_background` Bash tasks fire no end hook in the parent, so they are
  never counted as edges — the `Stop` payload is what reports them;
- a session on a remote host: its status channel holds one word and cannot
  count, so the edges are no-ops there. Its `Stop` is reduced in the shell to
  `background` (either list non-empty) or `done`; neither `pending` nor a
  proof crosses.

A permission prompt raised by a background subagent stores `blocked`; the
subagent's next tool call then stores `working`, and its end moves nothing,
so the session reads `working` until the next prompt — no worse than before
this state existed, when every such tool call stored `working`. A `blocked`
cannot say whose block it is, so it is not folded back to `background`.

A missed end edge is forgotten by the next prompt (`UserPromptSubmit` marks
its `working` as a turn start with `THURBOX_SIGNAL_TURN=start`; the main
agent's own tool calls, such as those of the turn that reports a finished
subagent back, forget nothing), by the agent starting again (`SessionStart` →
`idle`), by `session stop` and by a restart; `session get` (which probes the
pane by default) reports a dead pane as contradicting it. The interface does
not fold `background` by the terminal: the work runs out of the pane's sight,
and an interface that has not attached yet has no answer either. Because
`session signal` publishes to the pane's
status channel after it stores, a late `background` read back from that
channel — by the interface or by `automation tick`'s status poll — is refused
once the session's counted work has all ended.
