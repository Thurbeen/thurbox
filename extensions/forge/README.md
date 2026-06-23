# Forge — a workflow analyst that automates your thurbox for you

> **Status: experimental.** Forge is new and under active testing — expect the
> behavior spec, scripts, and installer to change between releases.

Forge watches how you actually use thurbox — your task backlog, the sessions
you keep spawning, your existing automations and how often they fire or fail —
and turns the **recurring patterns** into concrete, ready-to-apply proposals:
mostly new `thurbox-cli automation`s, sometimes a pointer to another extension
(`renovate`, `cve-watch`, `ci-shepherd`, …).

It **proposes, never imposes**. A scan only ever reads state and writes a
proposal file, so it is safe to run unattended on a schedule. Nothing is
created until you explicitly `apply` it.

```text
## Open
### Nightly cargo-deny advisory scan  `nightly-cve-scan`
- why: tasks #14 #19 #27 are all "check advisories" on thurbox (3× in 9 days)
- kind: automation

    thurbox-cli automation create --name cve-scan --trigger "cron:0 3 * * *" \
      --repo /home/me/repos/thurbox --agent claude \
      --prompt "Run cargo deny check advisories; open a task per new advisory."
```

Forge is **agent-agnostic**, like thurbox itself: the analyst is a plain
`agents.toml` entry (`forge`), so it can be claude, codex, antigravity, opencode,
vibe, … The behavior lives in [FORGE.md](FORGE.md), surfaced to whatever CLI
you pick via context-file symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` →
`FORGE.md`).

## Install

Forge is a thurbox **extension** (a declarative `extension.toml` manifest):

```bash
thurbox-cli extension install forge
```

or from a checkout: `thurbox-cli extension install ./extensions/forge`
(the `install.sh` / curl one-liner is a thin shim over this). Override the home
with `--home <dir>`. Installing:

1. lays down the forge home (`~/.config/thurbox/extensions/forge`): `FORGE.md` spec, helper scripts,
   context-file symlinks, claude permission settings, and an empty
   `proposals.jsonl`/`proposals.md` store (seed files are never clobbered on
   reinstall);
2. registers the `forge` agent in `~/.config/thurbox/agents.toml` (default:
   claude on sonnet — edit the entry to change the model/CLI);
3. activates the dedicated `forge` session and a `forge-scan` automation
   (weekly, Mondays 09:00) — both **self-healed** by thurbox at startup and on
   every tick, so deleting them is a no-op until you `deactivate`.

Re-running `extension install` refreshes the installer-owned files to the latest
version. `thurbox-cli extension list` / `status forge` show what's active.

## Use

- Forge scans on its schedule; trigger one now by sending `scan` to the forge
  session (or open it in the TUI).
- `status` for a one-screen list of open proposals.
- Review the rendered backlog in `~/.config/thurbox/extensions/forge/proposals.md`.
- `apply <slug>` runs the proposal's stored command (which always starts with
  `thurbox-cli`) and marks it applied. `dismiss <slug>` buries one so future
  scans won't resurface it.
- Anything else you type is treated as an ad-hoc question about your workflow.

## Files

| Path | Purpose |
|------|---------|
| `FORGE.md` | The agent behavior spec (modes, signal sources, proposal rubric) |
| `scripts/forge-snapshot.sh` | One-call view: tasks + sessions + automations (with run summaries) + open proposals |
| `scripts/proposals.sh` | The proposal store: `upsert` / `list` / `apply` / `dismiss` / `render` (JSONL → `proposals.md`) |
| `extension.toml` | Declarative install + runtime manifest (`thurbox-cli extension install` reads this) |
| `claude-settings.json` | Pre-approved permissions template (laid down as `.claude/settings.json`) |
| `install.sh` | Thin curl/sh shim over `thurbox-cli extension install` |

## How a proposal is applied safely

`proposals.jsonl` is the source of truth; `proposals.md` is the human render.
Every proposal carries an exact command, and `proposals.sh apply` **refuses to
run anything that does not start with `thurbox-cli `** — so applying a proposal
can only ever create/edit a thurbox automation, never run arbitrary shell.

## Uninstall

```bash
thurbox-cli extension deactivate forge        # off-switch: tears down session + automation
thurbox-cli extension uninstall forge         # also removes the agent + manifest
thurbox-cli extension uninstall forge --purge  # ...and deletes ~/.config/thurbox/extensions/forge (your proposals too)
```
