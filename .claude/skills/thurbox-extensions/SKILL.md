---
name: thurbox-extensions
description: Thurbox extensions: the shipped opt-in ones (flow, forge, ci-shepherd, renovate, ui-skill), the declarative extension.toml manifest format with its install and runtime halves, the three outside-reaching payload kinds, built-in embedded extensions and their opt-out, and the self-heal contract that recreates an active extension's sessions and automations. Use when writing, installing, debugging or removing an extension, or when a deleted session keeps coming back.
---

# Thurbox extensions

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Extensions

`extensions/` holds opt-in, **agent-agnostic** add-ons that build on
`thurbox-cli` without touching the core binary. Each ships an
`extension.toml` manifest installed via `thurbox-cli extension install
<name>` (with a thin curl-able `install.sh` shim over it).

- **`extensions/flow/`** *(experimental — new and under active testing)* — a
  focus-protecting triage agent: brain-dumps become thurbox tasks, dispatchable
  ones spawn worker sessions (on `flow/<slug>` worktree branches, agents
  `flow-worker`/`flow-worker-heavy` mapped in `agents.toml` to any CLI), a
  dedicated `flow` session monitors them, and every reply ends with the single
  next thing to focus on. Dispatch is **plan-first**: `scripts/create-task.sh`
  owns the worker prompt and injects a mandatory clarify → plan → build phase (≥3
  clarifying questions, then a written plan gated on user approval, then
  implement; seeded from `--accept`) so each worker plans before it codes. A dump
  spanning several `repos.md` repos becomes one **multi-repo** task:
  `create-task.sh` forwards `--add-repo PATH@origin/<base>` (own isolated
  worktree per repo) / `--add-dir PATH` to `task create`, and the worker opens a
  **separate PR per repo it changes** (its `result` carries `pr_urls`).
  Worker↔flow coordination is **event-driven over the
  [inter-session message queue](#inter-session-messages-mailbox-queue)**: a
  worker pushes `message send --to flow --kind questions|plan|result` (waking
  flow) with **no ids** (thurbox stamps sender + task from the injected
  `THURBOX_SESSION`/`THURBOX_TASK`); flow drains its inbox (`message inbox
  --claim`), surfaces the questions/plan under "Needs you", and relays the user's
  answer with `message reply <message_id>` — routed to that message's sender, so
  flow never maps a task to a session id (`flow-snapshot.sh` name-parsing is now
  human-board only). The worker drains its own inbox on the resulting `inbox`
  wake. Flow ships **no scheduled automation** — a **manual** `tick` is the
  janitor/safety-net (drain missed wakes, reset stale tasks, dispatch). The
  behavior spec is `FLOW.md`, surfaced to whichever CLI runs it via context-file
  symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). See
  `extensions/flow/README.md`.
- **`extensions/forge/`** *(experimental)* — a workflow analyst that mines
  your tasks/sessions/automations (and their run history) for **recurring
  patterns** and writes ready-to-apply `thurbox-cli automation` proposals. It
  **proposes, never imposes**: a scan (driven by a weekly `forge-scan`
  automation on the `forge` session) only reads state and writes
  `proposals.jsonl` (rendered to `proposals.md`); nothing is created until you
  `apply <slug>` — and `proposals.sh apply` refuses any command not starting
  with `thurbox-cli`. Spec: `FORGE.md`.
- **`extensions/ci-shepherd/`** *(experimental)* — watches your open change
  requests (GitHub PRs / GitLab MRs / Bitbucket PRs; repos in `repos.md`) and
  dispatches a `shepherd-worker` fixer for each one with **failing CI**, a
  **changes-requested review**, or a branch that is **behind its target**
  (needs rebase — the normalized `rebase` signal from `provider.sh`, surfaced
  as the `REBASE` action flag by `scripts/classify.sh`; `dispatch-fix.sh
  --rebase` makes the worker rebase onto the base and force-push before fixing).
  When **several PRs in one repo** are all REBASE-only, `classify.sh`
  **serializes** them — only the lowest-numbered keeps the live `REBASE` flag,
  the rest become `REBASE-QUEUED (behind #n)` — so the shepherd rebases one at a
  time (each merge advances the base for the next), clearing the stack in O(n)
  rebases instead of the O(n²) of force-pushing N mutually-invalidating branches.
  A `shepherd` session monitors via a `shepherd-tick` automation; fixers are
  thurbox **tasks** (`fix #<n>: …`) that self-report with the same `===RESULT===`
  sentinel as flow. It is **forge-agnostic**: only **git** is baked in; *how* to
  talk to a repo's host is decided by the shepherd agent each tick — built-in
  **fast paths** (github `gh`/gitlab `glab`/bitbucket REST via
  `scripts/provider.sh`) plus an **agent-driven** path for any other forge
  (`provider.sh describe` hands the agent the remote + installed clients; it
  lists the repo itself and passes `--branch`/`--checkout-cmd`/`--feedback-cmd`/
  `--comment-cmd` to `dispatch-fix.sh`). Because thurbox's `--worktree` always
  runs `git worktree add -b` (which fails on an existing branch),
  `dispatch-fix.sh` adopts the request branch itself into a shepherd-owned
  worktree. It is also **session-aware**: the snapshot joins each request's head
  branch against the live `thurbox-cli session list` (`scripts/link-sessions.sh`,
  pure + bats-tested). A request whose branch already has a **non-fixer** thurbox
  session (someone working it by hand) is **not** dispatched (two worktrees would
  force-push the same branch) but is **monitored and folded into the merge
  ordering** — that live session counts as the repo's active worker, so the other
  same-repo requests queue behind it. While such a request stays actionable the
  shepherd **nudges the live session** over the message queue (`thurbox-cli
  message send`) to do the rebase/merge — once per pending ask (guarded by
  peeking its unread inbox), not every tick — so the slot actually clears.
  Spec: `SHEPHERD.md`.
- **`extensions/renovate/`** *(experimental)* — keeps local repos on up-to-date
  dependencies. A `renovate` session sweeps a `repos.md` watch list on a weekly
  `renovate-tick` automation and dispatches a `renovate-worker` per eligible
  repo; the worker runs **Renovate's `local` platform only**
  (`scripts/renovate-run.sh` hard-codes `--platform=local` — no hosted bot, no
  token, no Renovate-opened PR), tests the result, commits to a fresh
  `renovate/updates-<ts>` branch, and opens a review PR. Updaters are thurbox
  **tasks** (`update <repo> deps …`) that self-report with the same
  `===RESULT===` sentinel as flow. Unlike ci-shepherd it starts a *new* branch,
  so `scripts/dispatch-update.sh` uses thurbox's native `--worktree` (no branch
  adoption). Version strategy is per-repo (`strategy` column: `patch`/`minor`/
  `major`/`all`, layered as a `RENOVATE_CONFIG` overlay) plus a global
  `renovate-config.json`. Spec: `RENOVATE.md`.
- **`extensions/ui-skill/`** *(built-in, on by default)* — the odd one out: it
  ships no session, no automation and no agent. It installs a single **agent
  skill**, `thurbox-ui`, into each coding CLI's *personal* skill directory
  (`~/.claude/skills/`, `~/.codex/skills/`, `~/.config/opencode/skills/`,
  `~/.copilot/skills/`, `~/.agents/skills/` — each guarded by `requires_dir`, so
  a CLI the user does not have is skipped), so an agent in **any** session knows
  how to change thurbox's own interface. It replaces the workaround of attaching
  the interface directory to every session as an extra repo: a skill loads only
  when the request is about the TUI, where an extra repo is in front of the agent
  always. Like `hooks` it is **embedded + auto-activated** (see below) — for the
  same reason: someone who does not already know the interface is editable will
  not go looking for the extension that says so. Opt out with `thurbox-cli
  extension deactivate ui-skill`. The payload is one `SKILL.md` — the short form
  of `ui/AGENTS.md` + `ui/README.md` — and it hard-codes no paths, opening with
  `thurbox-cli plugin dir` so one file is correct for a release build, a dev
  build and a `THURBOX_UI_DIR` override alike. Delivery is the ordinary
  `[[external_files]]` machinery, marker-guarded: `install`/`update`/`uninstall`
  act on thurbox's own copies and leave one the user has taken ownership of alone
  (drop the `Managed by` line and it is theirs), while `reinstall` and `install
  --force` overwrite as they do everywhere else.
> **Removed.** Four per-provider task-integration extensions
> (`github-issues`, `gitlab-issues`, `linear`, `jira`) lived here and were deleted:
> four near-identical trees, each carrying a provider's API shape, for a job that is
> a `curl` and an `upsert`. What made them possible is still in the binary and is
> deliberately provider-neutral (ADR-20 — no provider name in the binary): the
> `task --source/--external-id/--external-url` flags, `get_task_by_external_id`,
> the `idx_tasks_external` index, and the `Exec` automation action. A scheduled
> `Exec` running a script of your own does what they did.

### Extension manifests + self-heal (`thurbox-cli extension`)

Extensions stay **data, not binary** (ADR-20): core thurbox knows a declarative
**manifest format**, never a specific extension. Each extension ships an
`extension.toml` (`session::ExtensionDef`, pure data in
`session/extension_def.rs`; loaded by `agent::extension_config`) with two halves:
an **install** spec (`home`, `[[agents]]` to register in agents.toml, `[[files]]`
payload, `[[symlinks]]`, `[[external_files]]`, `[[agent_patches]]`,
`[[config_merges]]`) and a **runtime** spec (`[[sessions]]` + `[[automations]]` to
ensure/self-heal). The `{home}` token expands to the resolved home dir.

Three of those reach **outside** the extension home, all reversible:
`[[external_files]]` drops a managed file into an agent's own config dir (guarded
by `requires_dir`), `[[agent_patches]]` appends args to an existing agent in
agents.toml, and `[[config_merges]]` deep-merges shipped JSON into an agent's
*shared* config file (`agent::json_merge`; uninstall prunes by the
`thurbox-cli session signal` marker, so removal survives payload schema changes).

**Built-in extensions** (`session_ops::builtin`) — two of them, `hooks`
(`extensions/hooks/`) and `ui-skill` (`extensions/ui-skill/`), which unlike user
extensions ship **embedded** in the binary and are **auto-activated by default**
(`ensure_builtin_extensions` at TUI startup + headless tick). Each is a
`Builtin` — embedded assets, a home under *this build's* config dir, and how it
describes what it just did — and the shared `Builtin::ensure` materializes the
assets locally and installs them through the ordinary machinery above, so a
built-in is not a second installer with its own bugs. They exist for the same
reason: what they wire up has to be there before the user knows to ask for it.
`hooks` gives the default agent's status hook zero-setup, and `ui-skill` gives
whichever coding CLI the user runs the knowledge of how to edit the interface.
**Which hook delivery mechanism each built-in *agent* gets (and the exact states
each can report) is documented per agent in `docs/AGENTS.md` → "Status hook
mechanisms"** — that is the reference to update when adding an agent.
Remote sessions are provisioned by
`session_ops::remote_hooks::provision_agent_hooks_on_host`; a psmux/Windows host
is gated off (`session::psmux_hook_rewrite_supported`) and shows `Hooks:
degraded`. Opt out of either with `thurbox-cli extension deactivate <name>` (records a
`builtin_<name>_optout` metadata flag so self-heal won't resurrect it — the key
format is chosen so `hooks` keeps producing the `builtin_hooks_optout` row it
wrote before there was more than one built-in); `activate`/`install <name>`
clears it.

`thurbox-cli extension` (alias `ext`) — `install <name|url|dir>` / `uninstall` /
`reinstall` / `list` / `available` (alias `search`) / `update [--all] [--force]` /
`activate` / `deactivate` / `status`. A bare name resolves to the official source
**pinned to the binary's release tag**, so a fetched extension matches the binary.

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every active
extension at TUI startup (before session restore) and at the top of the headless
`automation tick`. Consequence worth knowing before debugging a "zombie" session:
while an extension is active, deleting its session/automation is a **no-op** —
they are recreated. `extension deactivate` is the real off-switch, and headless
healing needs `[features] automations = true`.

**Installer resolution order, payload flags, versioning/staleness
(`installed_with`/`is_stale`), and the full self-heal contract are in ADR-21 of
`docs/ARCHITECTURE.md`.**

