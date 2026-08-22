# ui-skill — teach any agent to edit thurbox's interface

thurbox's interface is Lua in a config directory of yours
(`thurbox-cli plugin dir` says which), so an agent working in some *other*
repository has no reason to know it exists. The usual workaround is to attach
that directory to every session as an extra repo, which puts it in front of the
agent whether or not the session is about the interface.

This extension does the same job the other way round: it installs one **agent
skill**, `thurbox-ui`, into each coding CLI's personal skill directory. The
agent loads it only when a request is actually about changing the TUI, and it
loads in **every** session — no extra repo, no per-session setup.

## It's on by default

Like `hooks`, **ui-skill ships built into thurbox and is auto-activated** on
first run — the skill is there before you know to ask for it, which is the point
(nobody goes looking for the extension that tells them the interface is
editable). Turn it off at any time:

```bash
thurbox-cli extension deactivate ui-skill   # removes every copy; won't come back
thurbox-cli extension activate ui-skill     # and back on
```

`deactivate` records an opt-out flag, so startup self-heal does not resurrect it.

Every destination is guarded, so a CLI you do not have installed is skipped
rather than having a config tree created for it:

| CLI | Destination | Written when |
|---|---|---|
| claude | `~/.claude/skills/thurbox-ui/SKILL.md` | `~/.claude` exists |
| codex | `~/.codex/skills/thurbox-ui/SKILL.md` | `~/.codex` exists |
| opencode | `~/.config/opencode/skills/thurbox-ui/SKILL.md` | `~/.config/opencode` exists |
| copilot | `~/.copilot/skills/thurbox-ui/SKILL.md` | `~/.copilot` exists |
| any | `~/.agents/skills/thurbox-ui/SKILL.md` | `~/.agents` exists |

opencode also reads `~/.claude/skills`, so it is covered even without its own
entry. The install itself reports which copies landed —
`extension install --json` carries `external_files_written` and
`external_files_skipped`, and the human summary counts them as `agent file(s)`.

Some CLIs cache their skill list for the life of a session — Copilot CLI wants
`/skills reload`, and a restart is the reliable answer everywhere else. Existing
thurbox sessions therefore pick the skill up on their next start.

## What the skill tells the agent

The short version of `<interface dir>/AGENTS.md` and `README.md`, plus the parts
that are easy to get wrong from outside the directory:

- **Find the directory first** (`thurbox-cli plugin dir`) — it is not the repo
  the session is in, and a thurbox *checkout's* `ui/` is not the live interface
  unless `THURBOX_UI_DIR` points at it.
- **Check every edit** with `thurbox-cli plugin check`, which catches the
  failure that looks like success: a pane that loads and draws nothing because
  no arrangement places its slot.
- **Adding a pane is two edits** — the plugin file and its slot in `layout.lua`.
- The sandbox (no `os`/`io`/`package`, no package managers, no building under a
  recursively-watched directory), the capability model, the theme roles, and the
  recovery paths for a broken interface.

It hard-codes no paths, so one payload is correct for a release build, a dev
build and a `THURBOX_UI_DIR` override alike.

## Editing it yourself

`SKILL.md` in `extensions/ui-skill/` is the single payload for every
destination, embedded in the binary by `session_ops::builtin_ui_skill`. The copy
under the extension home is not read — there is deliberately none laid down
there for that reason. To change what the skill says, edit that file and rebuild;
from a checkout, `thurbox-cli extension install ./extensions/ui-skill` installs
the edit without one.

Each delivered copy carries a `Managed by thurbox …` line. thurbox refreshes a
file that still has it and never touches one that does not, so **delete that
line in a copy to take ownership of it**. The exceptions are the two commands
whose job is to overwrite: `extension reinstall` and `extension install --force`
replace every copy, marker or no marker.

Note the corollary: a copy that still carries the line is refreshed *even if you
edited it*, since the line is the only thing distinguishing thurbox's file from
yours.

## Turning it off

```bash
thurbox-cli extension deactivate ui-skill
```

Every copy thurbox still owns (marker intact) is removed; a copy you took
ownership of is left alone. The now-empty `skills/thurbox-ui/` directories stay
behind — a documented leave-behind, so that removing this extension never walks
a tree of an agent's config that thurbox does not own.
