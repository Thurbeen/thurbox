---
name: thurbox-agents
description: The declarative coding-agent registry (agents.toml / AgentDef): the *_args groups, {id} and {home} substitution, session-id pinning vs resume_latest, the omp session-file kind, custom-agent status hooks via hook_schema, and multi-repo sessions with the symlink workspace. Use when adding or debugging a built-in or custom agent, changing spawn/resume/fork args, or working on multi-repo sessions.
---

# Thurbox agent definitions

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Agent Definitions

> Per-agent reference + the "adding a new built-in" checklist:
> `docs/AGENTS.md` (each built-in's exact config, ID model, and status-hook
> mechanism, plus every file to update when promoting a CLI to a built-in).

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`, `pi`, `omp`) on first run.
Each `[[agents]]` entry is an `AgentDef`:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = []                               # always passed; bake a model here if you want one
resume_args = ["--resume", "{id}"]      # emitted when resuming
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]  # emitted on a fresh spawn

[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]      # id-less: resumes the last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

Each `*_args` group is appended only when its driving value is
present, with `{id}` substituted; `args` is always passed. No
model is ever passed — each agent uses its own default config
(put `["--model", "opus"]` in `args` if you want to pin one).
A second token, `{home}`, expands (at spawn, on the spawn worker —
`session_ops::expand_home_in_def`, called from
`spawn::adapt_def_for_launch`, which both a fresh launch and
`session_ops::restart` go through) to the resolved home dir — the **remote** home for an
SSH/WSL host — so an agent that wants a session *file path* rather than a
bare id (the built-in `omp`, below) launches against a concrete,
quote-safe absolute path (a literal `~` would never expand — args are
POSIX-quoted).
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

**Session id pinning vs. `resume_latest`.** thurbox generates the
`agent_session_id` (a UUID) and `claude`/`pi` accept it at creation
(`--session-id {id}`), so only those two resume/fork by that exact id. The other
built-ins (`codex`, `opencode`, `antigravity`, `aider`, `copilot`) can't pin or
report their id, so they set `resume_latest = true` with **id-less** resume/fork
flags: the agent resolves "the last session in *this* directory" itself (`codex
resume --last`, `opencode --continue`, `agy --continue`, `aider
--restore-chat-history`, `copilot --continue`) — which works because restart
reuses the session's cwd and a single-repo fork reuses the parent's.
`resume_latest` only changes *when* the resume group fires
(`session_ops::resume_trigger_for`): for these agents restart always resumes; for
claude it defers to an on-disk transcript check. **`omp`** (Oh My Pi) is a third
kind: it generates its own internal id and won't take thurbox's, but its
`--session <path>` creates a fresh session at a missing path, so thurbox maps its
UUID to a deterministic file (`--session
{home}/.omp/agent/sessions/thurbox-{id}.jsonl` on create, `--resume` the same on
restart). Neither id-pinned nor `resume_latest`: `resume_trigger_for` resumes it
iff that JSONL exists (`session_file_template` — agent-neutral, keyed on a
`new_session_args` token that is a path *and* carries `{id}`, not on the agent
name); a remote-omp restart can't stat the host file from the UI thread, so it
starts fresh (documented fallback). Caveats: agents without `fork_args`
(`antigravity`, `aider`, `copilot`, `omp`) start fresh on `Ctrl+F`; and a
**multi-repo** fork of a cwd-scoped agent lands in a fresh symlink workspace, so
`--last`/`--continue` finds no parent session (multi-repo *restart* still resumes,
keeping the same workspace dir).

- **Data type**: `session::AgentDef` / `session::AgentRegistry`
  (`session/agent_def.rs`, pure data + substitution logic).
- **Loading**: `agent::agent_config::load_or_seed()` reads/seeds
  the TOML; `builtin_registry()` is the fallback.
- **Launching**: `agent::GenericProvider` wraps an `AgentDef` and
  implements the `AgentProvider` trait (`command()` +
  `build_args(&SessionConfig)`). It is constructed from the session's
  `AgentDef` at each launch site — `session_ops` for a spawn or restart,
  `kernel::terminal` when a pane adopts one.

A session stores only its **agent name**; there are no
per-session model/permission/prompt/tool knobs.

**Custom-agent status hooks (`hook_schema`).** thurbox stays agent-neutral, so
the built-in **hooks** extension wires status hooks only for the built-ins it
knows by name — a **custom** agent (e.g. a rebranded-claude `fleet`) gets no
`--settings` patch and so never reports working/blocked/done. An optional
`AgentDef.hook_schema: Option<String>` closes this: it names the hook **family**
the CLI speaks, and `agent::extension_config::apply_agent_patches` (+ its
uninstall reverse) fans each `[[agent_patches]]` out to the built-in named
`patch.name` **and** every agent with `hook_schema == patch.name`. So
`hook_schema = "claude"` on `fleet` injects the same `--settings {home}/
claude.json` claude gets — and, because the remote rewrite
(`session_ops::spawn::adapt_agent_args_for_remote`) keys off the `--settings`
arg rather than the agent name, remote/WSL wiring follows for free. Only the
per-arg-patch families (claude, aider) need it; codex/opencode/antigravity/vibe/
copilot wire through their own config dir, so a rebrand sharing that dir already
reports. thurbox bakes in no agent knowledge — the *user* asserts the family.

### Multi-repo sessions (symlink workspace)

A session can span several repositories (the repo picker allows multiple;
headless callers pass `--add-repo`/`--add-dir`, below). Because agent CLIs differ
wildly in how — or whether — they accept extra directories, thurbox passes **no**
per-agent `--add-dir`-style flags. Instead, a session with more than one member
directory launches in a per-session **symlink workspace**:
`~/.local/share/thurbox/workspaces/<agent_session_id>/` holds one symlink per repo
(worktree checkout or plain dir) and the agent starts there (`cwd` = the
workspace), so every agent sees each repo as a subdirectory — agent-neutral, no
`agents.toml` changes.

`SessionInfo.cwd` keeps the **primary** repo (display / editor / git context); the
workspace is a spawn-time process-cwd detail, derived idempotently on every launch
from the persisted members and never stored. `workspace::ensure_workspace` /
`remove_workspace` (`src/workspace.rs`) build and tear it down; the member set is
the single `App::session_member_dirs` list that also feeds the rendered repo
names, and `App::resolve_process_cwd` picks workspace-vs-primary. Single-repo
sessions are unchanged (`cwd` = the repo directly).

**Headless multi-repo.** The same multi-repo shape is reachable without the
TUI: `SpawnRequest.extra_repos: Vec<ExtraRepo>` (`session/automation.rs`) carries
each additional repo, where `ExtraRepo { repo_path, worktree: bool, base_branch }`
either gets its **own isolated worktree** on the spawn's shared `worktree_branch`
(off its own base — the per-repo-PR model flow uses) or is attached **as-is** as
an additional dir. `session_ops::spawn::resolve_dirs` builds the worktrees +
additional dirs and `resolve_launch_cwd` mirrors the TUI's `resolve_process_cwd`
(symlink workspace when ≥2 members). The CLI exposes it on `session create` and
`task create` via repeatable `--add-repo PATH[@BASE]` (worktree) and `--add-dir
PATH` (as-is); `AutomationAction::Spawn` persists the list as JSON in the
`action_extra_repos` column (schema v33, on both `tasks` and `automations`;
`NULL`/empty = single-repo, so old rows are byte-identical). The flow extension's
`create-task.sh` forwards these flags (see `extensions/flow/FLOW.md`).

