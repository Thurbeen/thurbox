# Built-in Agents Reference

The exact configuration and runtime behavior of every **built-in** coding
agent, plus the checklist for **adding a new one**.

Thurbox is agent-neutral: it knows nothing about a coding agent's model,
prompts, permissions, or tools — only how to launch the CLI and how that CLI
resumes, forks, and reports status. Every agent is **data**, not code. The
built-ins are just the entries thurbox ships pre-seeded; a user adds their own
by editing `agents.toml` (see [CONFIG.md](CONFIG.md#agentstoml) for the file
format). This document is the single source of truth for **what each built-in's
data actually is and why**, and it exists because adding a new built-in touches
several files that are *not* updated automatically — see
[Adding a new built-in agent](#adding-a-new-built-in-agent).

Related docs:

- [CONFIG.md → agents.toml](CONFIG.md#agentstoml) — the config-file format and
  per-field syntax (`{id}` substitution, `resume_latest`, `hook_schema`).
- [FEATURES.md → Agent definitions](FEATURES.md#agent-definitions) — the
  agents-as-data design.
- [ARCHITECTURE.md](ARCHITECTURE.md) — the `session::AgentDef` purity rule.
- `../CLAUDE.md` → *Agent Definitions* — the in-repo working reference.

## Where the built-ins live

The built-in registry is **not** hand-constructed in Rust. It is an embedded
TOML document, `BUILTIN_AGENTS_TOML`, in
[`src/agent/agent_config.rs`](../src/agent/agent_config.rs). `builtin_registry()`
parses that string, and `seed_agents_toml()` writes it verbatim to
`~/.config/thurbox/agents.toml` on first run. So the same text is both the
in-binary fallback and the seed the user then edits — keep it authoritative.

Status hooks (working / blocked / done) are wired **separately** by the built-in
**hooks** extension, whose per-agent mechanism is declared in
[`extensions/hooks/extension.toml`](../extensions/hooks/extension.toml). An
`AgentDef` says *how to launch* an agent; the hooks manifest says *how that agent
reports state back*. Adding a built-in agent means touching **both**.

## The built-in agents

Nine agents ship pre-seeded. `default = "claude"`.

| Name | Command | Resume | Fork | ID model | Status hooks |
|------|---------|--------|------|----------|--------------|
| `claude` | `claude` | `--resume {id}` | `--resume {id} --fork-session` | **pinned** (`--session-id {id}`) | `--settings` arg patch (`claude.json`) |
| `codex` | `codex` | `resume --last` | `fork --last` | id-less (`resume_latest`) | `config_merges` → `~/.codex/hooks.json` |
| `antigravity` | `agy` | `--continue` | — (none) | id-less (`resume_latest`) | `config_merges` → `~/.gemini/settings.json` |
| `opencode` | `opencode` | `--continue` | `--continue --fork` | id-less (`resume_latest`) | `external_files` → `~/.config/opencode/plugin/` |
| `aider` | `aider` | `--restore-chat-history` | — (none) | id-less (`resume_latest`) | `--notifications-command` arg patch (blocked only) |
| `copilot` | `copilot` | `--continue` | — (none) | id-less (`resume_latest`) | `external_files` → `~/.copilot/hooks/` |
| `vibe` | `vibe` | — (starts fresh) | — (none) | none | `external_files` → `~/.vibe/hooks.toml` |
| `pi` | `pi` | `--session-id {id}` | `--fork {id}` | **pinned** (`--session-id {id}`) | `external_files` → `~/.pi/agent/extensions/` |
| `omp` | `omp` | `--resume {home}/.omp/…/thurbox-{id}.jsonl` | — (none) | **path-pinned** (`--session <file>`) | `external_files` → `~/.omp/agent/extensions/` |

### ID model: pinned vs. id-less

This is the single most important behavioral distinction, because it decides
whether resume/fork can target *this* session or only *the last* one.

- **Pinned** (`claude`, `pi`) — the CLI accepts a thurbox-generated UUID at
  creation via `new_session_args = ["--session-id", "{id}"]`, so it can
  resume/fork **that exact session** by id. `resume_latest` stays `false`.
- **Path-pinned** (`omp`) — the CLI generates its *own* internal session id and
  can't accept thurbox's, but it takes a session **file path**. thurbox maps its
  UUID to a deterministic JSONL under OMP's default root
  (`--session {home}/.omp/agent/sessions/thurbox-{id}.jsonl` on create,
  `--resume` on the same path to restore), so restart reattaches the exact
  session even though the id itself is opaque. The new `{home}` token is expanded
  to the resolved home dir **at spawn time by thurbox** (args are POSIX-quoted, so
  a literal `~` would never expand), and it translates onto the remote/WSL home.
  It has no `fork_args` (OMP can't pin a fork's target file to a thurbox UUID), so
  `Ctrl+F` starts fresh.
- **id-less** (`codex`, `antigravity`, `opencode`, `aider`, `copilot`) — the CLI
  can neither pin nor report a session id, so the agent's own flags resolve *"the
  last session in this directory"* (`codex resume --last`, `--continue`,
  `--restore-chat-history`). These entries set `resume_latest = true` and use no
  `{id}` token. This works because restart reuses the session's cwd and a
  single-repo fork reuses the parent's cwd. **Caveat:** a *multi-repo* fork lands
  in a fresh symlink workspace, so `--last`/`--continue` finds no parent session
  there (multi-repo *restart* still works — same workspace dir).
- **none** (`vibe`) — no resume group at all, so it always starts fresh on
  restart. The live tmux process is what carries its state across TUI restarts.

`resume_latest` only changes **when** the resume group fires
(`session_ops::resume_trigger_for`): for id-less agents restart always triggers
resume; for `claude` it defers to an on-disk transcript check.

### Fork

Agents that declare no `fork_args` (`antigravity`, `aider`, `copilot`, `vibe`,
`omp`) cannot fork — `Ctrl+F` starts a **fresh** session instead. None of those
CLIs support forking a conversation to a thurbox-pinned target.

### Status hook mechanisms

Every built-in reports status via the **hooks** extension, but *how* the hook is
delivered differs by what each CLI supports. All are declared in
[`extensions/hooks/extension.toml`](../extensions/hooks/extension.toml); the
embedded hook assets live in
[`extensions/hooks/`](../extensions/hooks/) and are `include_str!`'d by
[`src/session_ops/builtin_hooks.rs`](../src/session_ops/builtin_hooks.rs).

- **`agent_patches` (arg injection)** — appends args to the agent's launch
  command, reversibly.
  - `claude`: `--settings {home}/claude.json` (claude *merges* it with the user's
    own settings, never clobbering). Full lifecycle: idle/working/blocked/done.
  - `aider`: `--notifications-command "thurbox-cli session signal --state
    blocked"`. aider has only a "waiting for input" callback, so **blocked is the
    only state it can report**.
- **`config_merges` (reversible JSON deep-merge into a shared config file)** —
  for agents whose hooks live in a file thurbox must not overwrite.
  - `codex`: merged into `~/.codex/hooks.json` (SessionStart→idle,
    UserPromptSubmit/PreToolUse→working, Stop→done; **no blocked**). *Experimental.*
  - `antigravity` (`agy`): merged into the shared `~/.gemini/settings.json` (agy
    adopted claude's hook schema — PreToolUse→working, Notification→blocked,
    Stop→done; no UserPromptSubmit, so working fires at the first tool call).
- **`external_files` (drop a standalone managed file into the agent's config
  dir)** — refused if a non-managed file already exists there (the agent goes
  *unreported*, never *broken*).
  - `opencode`: a plugin at `~/.config/opencode/plugin/thurbox-status.js`
    (idle/working/blocked/done).
  - `copilot`: `~/.copilot/hooks/thurbox-status.json`
    (sessionStart→idle, userPromptSubmitted/preToolUse→working, agentStop→done,
    notification matched to `permission_prompt`→blocked). Ships both `bash` and
    `powershell` commands.
    *Experimental.*
  - `vibe`: a managed `~/.vibe/hooks.toml` (pre_tool→working, post_agent→done;
    **no blocked** — vibe has no permission/notification hook). Verified against
    vibe 2.21.0.
  - `pi`: a TypeScript extension at `~/.pi/agent/extensions/thurbox-status.ts`
    (session_start→idle, agent_start/tool_execution_start→working, agent_end→done;
    **blocked inferred only** from an `ask_user_question` tool call). *Experimental.*
  - `omp`: a TypeScript extension at `~/.omp/agent/extensions/thurbox-status.ts`,
    mirroring pi's but mapping OMP's structured user-question tool — named `ask`
    — to blocked (it recognizes both `ask` and pi's `ask_user_question`). Verified
    against OMP 17.0.6. *Experimental.*

Each `requires_dir` guard makes the drop a no-op when that agent isn't installed,
so a fresh install with only claude present doesn't scatter files for agents you
don't have.

**On a shared host** (`hosts.toml` `share_sessions = true`, the default — see
ADR-24) none of the remote rewriting applies: the host's own `thurbox-cli`
launches the agent with the host's own hooks extension, so each agent reports
exactly as it does locally, into the host's database, which the remote thurbox
mirrors. That includes a **Windows (psmux) host**, whose remote path stays
gated (`psmux_hook_rewrite_supported`) but is simply not used when the host
has — or is provisioned with — a CLI.

### `hook_schema` (custom rebrands only)

The built-ins never set `hook_schema` — the hooks extension already knows them by
name. It exists for a **user's** custom agent that runs a built-in under a
different name (e.g. a rebranded-claude CLI called `fleet`): set `hook_schema =
"claude"` and it inherits claude's `--settings` hook wiring under its own name.
Only the per-arg-patch families (`claude`, `aider`) need it; the config-dir
agents (codex/opencode/antigravity/vibe/copilot/pi) wire through their own config
dir, so a rebrand sharing that dir already reports without it. See
[CONFIG.md](CONFIG.md#agentstoml) and the `AgentDef.hook_schema` doc comment.

## Adding a new built-in agent

Adding support for a new CLI as a **user** needs only an `agents.toml` edit — no
recompile (that's the whole point of agents-as-data). But promoting a CLI to a
shipped **built-in** touches several places that are **not** kept in sync
automatically. Work the checklist top to bottom:

1. **Seed entry** — add an `[[agents]]` block to `BUILTIN_AGENTS_TOML` in
   [`src/agent/agent_config.rs`](../src/agent/agent_config.rs). Set `command`,
   the resume/fork/new-session groups, and `resume_latest` per the ID model
   above. Add a short comment explaining the CLI's resume/fork semantics, as the
   existing entries do. **Decide the ID model first** — whether the CLI can pin a
   session id — because it determines every `*_args` group.

2. **Status hooks** — add the wiring to
   [`extensions/hooks/extension.toml`](../extensions/hooks/extension.toml) and
   the hook asset file under [`extensions/hooks/`](../extensions/hooks/). Pick the
   mechanism from [Status hook mechanisms](#status-hook-mechanisms):
   `agent_patches` if hooks are launch flags, `config_merges` if they live in a
   shared config file thurbox must not clobber, `external_files` for a standalone
   drop into the agent's own config dir. Register any new embedded asset with an
   `include_str!` in
   [`src/session_ops/builtin_hooks.rs`](../src/session_ops/builtin_hooks.rs).
   Bump the hooks extension `version`. Skipping this step means the new agent
   launches fine but **never shows working/blocked/done**.

3. **Docs — update every list of built-ins** (these are prose, not generated, so
   they drift):
   - [The built-in agents](#the-built-in-agents) table above.
   - [CONFIG.md → agents.toml](CONFIG.md#agentstoml) — the parenthetical list and
     the "N built-ins" count.
   - [FEATURES.md → Agent definitions](FEATURES.md#agent-definitions) — the prose
     list and the pinned-id sentence.
   - `../CLAUDE.md` → *Agent Definitions* and *Custom-agent status hooks* — the
     built-in lists and the resume/fork caveats.

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
