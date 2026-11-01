# sessions-skill — teach any agent to work with other sessions

Every thurbox session is an agent in its own terminal, and `thurbox-cli` can
list them, read their screens, and message them. An agent does not know that
until something tells it, and so a request like "ask the backend session what it
changed" gets an agent guessing at tmux.

This extension installs one **agent skill**, `thurbox-sessions`, into each
coding CLI's personal skill directory. The agent loads it only when a request is
about another session, in **every** session — no per-session setup.

## It's on by default

Like `hooks` and `ui-skill`, **sessions-skill ships built into thurbox and is
auto-activated** on first run. Turn it off at any time:

```bash
thurbox-cli extension deactivate sessions-skill   # removes every copy; won't come back
thurbox-cli extension activate sessions-skill     # and back on
```

`deactivate` records an opt-out flag, so startup self-heal does not resurrect it.
Each built-in has its own flag: turning this one off leaves `ui-skill` alone.

Every destination is guarded, so a CLI you do not have installed is skipped
rather than having a config tree created for it:

| CLI | Destination | Written when |
|---|---|---|
| claude | `~/.claude/skills/thurbox-sessions/SKILL.md` | `~/.claude` exists |
| codex | `~/.codex/skills/thurbox-sessions/SKILL.md` | `~/.codex` exists |
| opencode | `~/.config/opencode/skills/thurbox-sessions/SKILL.md` | `~/.config/opencode` exists |
| copilot | `~/.copilot/skills/thurbox-sessions/SKILL.md` | `~/.copilot` exists |
| any | `~/.agents/skills/thurbox-sessions/SKILL.md` | `~/.agents` exists |

Some CLIs cache their skill list for the life of a session, so existing thurbox
sessions pick the skill up on their next start.

## What the skill tells the agent

Only commands `thurbox-cli` already has:

- **Find** sessions and itself — `session list` (`--fields`, `--tree`), the
  `THURBOX_SESSION` variable every session carries, and why a name is not an id.
- **Read** without disturbing — `session get` (state, worktree, branch, host),
  `session capture` (the screen), `session exec` for read-only commands in the
  session's directory, and what each `state` means for busy versus idle.
- **Send** safely — `message send`/`inbox`/`reply` first, which never type into
  a pane; `session send` only on request, after confirming the target, that it
  is idle, and that it is not the session open in the user's TUI
  (`ui state` → `selected_session`).
- **Never** without being asked — interrupting keys, `--force`, `session
  focus`, or any lifecycle verb (`delete`, `restart`, `stop`, …) on someone
  else's session.

## Editing it yourself

`SKILL.md` in `extensions/sessions-skill/` is the single payload for every
destination, embedded in the binary by `session_ops::builtin_sessions_skill`.
To change what the skill says, edit that file and rebuild; from a checkout,
`thurbox-cli extension install ./extensions/sessions-skill` installs the edit
without one. `tests/sessions_skill_e2e.rs` checks that every `thurbox-cli`
command the skill names exists.

Each delivered copy carries a `Managed by thurbox …` line. thurbox refreshes a
file that still has it and never touches one that does not, so **delete that
line in a copy to take ownership of it**. `extension reinstall` and `extension
install --force` replace every copy regardless.

## Turning it off

```bash
thurbox-cli extension deactivate sessions-skill
```

Every copy thurbox still owns (marker intact) is removed; a copy you took
ownership of is left alone. The now-empty `skills/thurbox-sessions/` directories
stay behind, so that removing this extension never walks a tree of an agent's
config that thurbox does not own.
