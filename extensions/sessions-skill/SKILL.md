---
name: thurbox-sessions
description: Work with the other thurbox sessions on this machine — find a session by name, project, branch or host, read what it is doing (its screen, state, worktree and branch) without disturbing it, and hand it a message or a prompt safely. Use when the user mentions another session, agent or worker ("ask the backend session", "what is the other agent doing", "tell the worker to…"), wants context pulled from one, or wants work coordinated between sessions.
---

# Working with other thurbox sessions

> Managed by thurbox `extension install` — see **Updating this skill** at the
> bottom before editing it.

Every thurbox session is an agent in its own terminal, and `thurbox-cli` reaches
all of them. Output is TOON (compact) when piped; add `--json` for every field.

**Read freely; write only when the user asked for it.** Reading never touches
the other session. Typing into it, interrupting it, or changing its lifecycle
acts on someone else's work.

## Find sessions, and yourself

```bash
thurbox-cli session list                    # name, agent, state, id
thurbox-cli session list --fields name,state,host,cwd,id
thurbox-cli session list --tree             # leads with their workers nested
echo "$THURBOX_SESSION"                     # your own session id (unset outside thurbox)
```

A reference is a UUID, an exact name, or a unique id prefix. **Names are not
unique** — an ambiguous one exits 3 and names both ids. Once you have picked a
session, use its id. Never act on yourself by mistake: skip the row whose `id`
is `$THURBOX_SESSION`. `host` is `null` for this machine; otherwise the session
runs on that `hosts.toml` host, and the same commands reach it there.

## Read a session's context (non-disturbing)

```bash
thurbox-cli session get <id>                 # state, cwd, worktrees[].branch, host, last_output_at, transcript_path
thurbox-cli session capture <id> --lines 80  # what its screen shows now (max 10000)
thurbox-cli session exec <id> -- git status -sb   # a separate process in its directory, on its machine
```

`capture` is a snapshot of the rendered pane plus `foreground_command`; it never
types anything. `exec` does not touch the pane either — run only read-only
commands with it. `transcript_path`, when set, is the agent's own conversation
log, readable as a file.

**State** (`session get`'s `state`): `working` — mid-turn, busy; `blocked` —
waiting on a person (a permission prompt, a question); `idle`/`done` — between
turns; `stopped` — parked, refuses input; `unreachable` — its host is down;
`running`/`uncovered`/`unreported` — no status hooks, so busy and idle cannot
be told apart: read the screen with `capture`.

## Send something

Prefer a **message** — it never types into the pane:

```bash
thurbox-cli message send --to <id> --kind note --body "…"   # you are stamped as the sender
thurbox-cli message inbox                                   # your own unread mail
thurbox-cli message reply <message_id> --body "…"            # answer, without knowing the sender's id
```

The result's `delivered_via` says where it went: `claude-socket` or
`codex-queue` reach the agent itself; `mailbox` only queues it until that
session runs `message inbox` — tell the user so. `may_hold: true` means a
Claude Code recipient may show an approval dialog before reading it.

**Typing a prompt** (`session send`) is for when the user asked you to drive
that session. Before it:

1. Confirm the target: `session get <id>` — the name, cwd and branch you expect,
   not yourself, not `stopped`.
2. Check it is not busy: state `idle`/`done`, or a `capture` showing an empty
   prompt. Do not send into `working` or `blocked` — a `blocked` session is
   waiting for a person's answer, not yours. To wait: `thurbox-cli session wait
   <id> --state idle,done --timeout 600`.
3. Check the user is not looking at it: `thurbox-cli ui state --json` →
   `selected_session` is the session open in their TUI. If it is your target,
   send a message instead, or ask.

```bash
thurbox-cli session send <id> "Summarise your progress in three lines"
thurbox-cli session send <id> "…" --wait-turn --timeout 600   # and wait for it to start working
```

`send` types only into an empty input line. If someone is typing there it does
not overwrite them: it waits briefly, then hands the text over as a message —
`sent: false`, `delivered_via` names the route, and the exit is non-zero when it
only reached the mailbox. Never add `--force` to get past that — the
text in the box is a person's. `submitted: false` means the text is still
sitting unsent in the box.

## Never, unless the user explicitly asks

- `session key` (`escape`, `ctrl-c` interrupt a turn), `--force`, or
  `session focus` (switches the user's screen).
- Anything that changes another session's lifecycle or identity: `delete`,
  `restart`, `stop`, `start`, `fork`, `rename`, `reparent`, `set-key`, `meta set`.
- Answering another session's permission prompt or dialog for it.
- Sending secrets, or more of your context than the task needs.

Exit codes: 1 not found, 3 ambiguous reference, 4 a wait timed out, 6 the
session was deleted, stopped or lost while waiting.

## Updating this skill

thurbox installs this file itself — the `sessions-skill` extension is built into
the binary and on by default — and refreshes it on every start. Deleting the
"Managed by" line near the top makes the copy yours: thurbox then leaves it
alone. (`extension reinstall` and `extension install --force` overwrite
regardless — that is what they are for.) Remove every copy thurbox still owns,
for good, with `thurbox-cli extension deactivate sessions-skill`.
