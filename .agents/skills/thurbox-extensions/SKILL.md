---
name: thurbox-extensions
description: Thurbox extensions: the declarative extension.toml manifest format with its install and runtime halves, the three outside-reaching payload kinds, the two built-in embedded extensions (hooks, ui-skill) and their opt-out, the fleet control plane as the worked out-of-repo example, and the self-heal contract that recreates an active extension's sessions and automations. Use when writing, installing, debugging or removing an extension, or when a deleted session keeps coming back.
---

# Thurbox extensions

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Extensions

An extension is an opt-in, **agent-agnostic** add-on that builds on
`thurbox-cli` without touching the core binary: an `extension.toml` manifest
plus whatever files it lays down, installed with `thurbox-cli extension install
<url|dir|git+repo|name>`.

`extensions/` in this repo holds **only the two built-ins**, `hooks` and
`ui-skill`. Both ship embedded in the binary and auto-activate, so neither is
something a user installs by name — which is why
`agent::extension_config::OFFICIAL_EXTENSIONS`, the bare-name registry behind
`extension available` and the typo hints, is currently **empty**. The mechanism
is intact; there is simply nothing that resolves by bare name. Anything else
installs from a URL, a local path, or a repository (`git+https://...`).

- **`extensions/hooks/`** *(built-in, on by default)* — status-hook delivery for
  the built-in agents, so the default agent's session status works with zero
  setup. Its assets are `include_str!`d by `session_ops::builtin_hooks`, so
  **the directory is load-bearing: delete a file and the binary stops
  compiling.** Which hook mechanism each agent gets, and the states each can
  report, is per-agent in `docs/AGENTS.md` → "Status hook mechanisms".
- **`extensions/ui-skill/`** *(built-in, on by default)* — it ships no session,
  no automation and no agent. It installs a single **agent skill**,
  `thurbox-ui`, into each coding CLI's *personal* skill directory
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

### fleet — the worked example of a real extension

The one extension the docs present to a user lives in **another repo**:
[Thurbeen/fleet](https://github.com/Thurbeen/fleet), a control-plane template
you clone. It is the reference for what a manifest looks like in practice —
`[[agents]]`, one `[[files]]` payload, three `[[symlinks]]` surfacing it as
`CLAUDE.md`/`AGENTS.md`/`GEMINI.md`, one long-lived `[[sessions]]`, and
deliberately no `[[automations]]` — and it ships its manifest as
`extension.toml.in` with a `__REPO_PATH__` placeholder its
`scripts/install-extension.sh` renders, because `{home}` resolves to the
extension home and no token spells "my clone". `docs/ORCHESTRATION.md` → "The
reference implementation" owns that story; nothing in thurbox knows fleet
exists.

> **Removed.** Four opt-in extensions (`flow`, `forge`, `ci-shepherd`,
> `renovate`) lived under `extensions/` and were deleted, unused. Earlier still,
> four per-provider task-integration extensions (`github-issues`,
> `gitlab-issues`, `linear`, `jira`) went the same way: four near-identical
> trees, each carrying a provider's API shape, for a job that is a `curl` and an
> `upsert`. What made all of them possible is still in the binary and is
> deliberately provider-neutral (ADR-20 — no provider name in the binary): the
> manifest format below, the `task --source/--external-id/--external-url` flags,
> `get_task_by_external_id`, the `idx_tasks_external` index, the `Exec`
> automation action, and the inter-session message queue. A scheduled `Exec`
> running a script of your own does what they did.

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
agents.toml, and `[[config_merges]]` deep-merges a shipped document into an
agent's *shared* config file (`agent::json_merge`, or `agent::toml_merge` when
the entry sets `format = "toml"` for a TOML config such as kimi's
`~/.kimi-code/config.toml` — `toml_edit`, so the user's comments and key order
survive; JSON prunes by the `thurbox-cli session signal` marker in an entry's
content, TOML by an ownership comment on the entry itself — which is why the TOML
payload stamps every entry with one, and why a user hook that calls `session
signal` survives uninstall there but would not in JSON).

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

`thurbox-cli extension` (alias `ext`) — `install <url|dir|git+repo|name>` /
`uninstall` / `reinstall` / `list` / `available` (alias `search`) / `update
[--all] [--force]` / `activate` / `deactivate` / `status`. A bare name resolves
to the official source **pinned to the binary's release tag**, so a fetched
extension matches the binary — which is the reason the mechanism stays even with
`OFFICIAL_EXTENSIONS` empty. With nothing to list, `available` and a failed
bare-name install both name the three forms that do work instead
(`extension_config::NO_BARE_NAME_HELP`).

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every active
extension at TUI startup (before session restore) and at the top of the headless
`automation tick`. Consequence worth knowing before debugging a "zombie" session:
while an extension is active, deleting its session/automation is a **no-op** —
they are recreated. `extension deactivate` is the real off-switch, and headless
healing needs `[features] automations = true`.

**Installer resolution order, payload flags, versioning/staleness
(`installed_with`/`is_stale`), and the full self-heal contract are in ADR-21 of
`docs/ARCHITECTURE.md`.**

