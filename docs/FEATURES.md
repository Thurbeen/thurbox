# Feature Decisions

Design rationale for user-facing behavior.
For architectural choices, see [ARCHITECTURE.md](ARCHITECTURE.md).

> **Read this first.** Much of the reasoning below was written while the interface
> was Rust (v1). The interface is now a Lua plugin kernel (ADR-23), so a section may
> describe a surface that no longer exists, or one that still exists in a plugin
> rather than in `src/ui`. Sections in the first case say so at the top; the
> *rationale* is kept either way, because it is what a plugin rebuilding that
> surface needs and it is not recoverable from the code.
>
> The engine below the interface — sessions, worktrees, agents, hosts, storage,
> extensions, the CLI — is unchanged, and so are those sections.

---

## Session Sidebar

### Single session list

The left sidebar holds a single flat list of sessions — there is no
project grouping layer above it. Sessions are top-level, identified
by a UUID v4, and labeled with their name, agent, branch (when in a
worktree), and cwd.

**Why no projects?**

- An earlier design grouped sessions under projects (one project →
  many sessions, with shared repos). In practice users tended to
  create one session per task, so the project layer was pure
  overhead: an extra navigation level, an extra creation step, and
  an extra deletion guard.
- Removing the project layer (storage migration v16 dropped
  `projects` and `project_repos`) collapses the model to "sessions
  own their own configuration". Each session picks its own agent
  and repos at creation time.

**Why a sidebar at all instead of a popup?**

- Sessions are persistent context, not transient selections. An
  always-visible list shows each session's status and live agent
  activity at a glance — useful for monitoring multiple parallel
  agent sessions.
- The sidebar fits cleanly into the existing 3-tier responsive
  layout (`<80`, `>=80`, `>=120`); a popup would require its own
  open/close keybinding and dismissal logic.

### Fuzzy search

Searching is unified into the **global search** (`Ctrl+/`) — see the
*Global Search* section below. There is no separate per-list `/`
filter; instead the global strip highlights matches live across the
session list, tasks panel, and automations pane at once. Sessions are
matched on name, agent, branch name, and cwd.

**Why all four fields?** Users remember sessions by whichever
attribute is most distinctive — sometimes the branch name, often
the agent ("the codex one"), occasionally the repo path. Indexing
all four makes the search hit on the first attempt without forcing
the user to remember which field to type into.

### Live status & "needs attention"

Each row is a single line: `<status-dot> <name> [<agent-status>]`
(worktree sessions get a `⑂` mark before the name). The agent's live
activity title (the OSC `0`/`1`/`2` window title it sets, e.g. Gemini's
`◇ Ready`) is appended after the name when present, muted and truncated
with `…` to fit the panel. The repo/branch and agent live in the info
panel, not the list row.

The colored **status dot** is driven by **agent hooks**, not output
heuristics. Each agent CLI's lifecycle hooks call `thurbox-cli session
signal --state <working|blocked|done|idle>` (identity from the injected
`THURBOX_SESSION`), and `refresh_session_statuses` maps the persisted
state to one of six `SessionStatus` values once per tick:

| State | Colour | Glyph | Meaning |
|-------|--------|-------|---------|
| `Working` | yellow | braille spinner (`⠋⠙⠹…`; static `◐`) | agent is actively running |
| `Blocked` | red | `◆` | agent needs input or approval |
| `Done` | blue | `●` | a turn just finished; shown until you switch away |
| `Idle` | green | `○` | acknowledged, never active, or at rest |
| `Error` | red | `✗` | reserved for a crashed agent (not derived yet) |
| `Unreachable` | muted grey | `⊘` | remote host is down/offline; placeholder row awaiting reconnect |

A `Done` session becomes `Idle` once you move focus off it (you've
acknowledged it); a `working` session that goes quiet for 10 s is
treated as `Idle` so an interrupted turn never spins forever. A remote
session whose host is unreachable is shown as a **placeholder** tagged
`Unreachable` — it never silently vanishes from the list, and the host
is retried in the background (or on demand via restart) until the session
reconnects and adopts in place. This covers both a host that is down at
restore *and* a live session whose host dies mid-run (detected via the
control-mode connection dropping). Status only **recolors** the dot — the
manual order is never disturbed (see *Smart ordering* below). Repo groups
roll up to their most-urgent member
(`Blocked > Error > Working > Done > Unreachable > Idle`).

The hooks are wired automatically by the built-in **hooks** extension
(auto-activated on first run; opt out with `thurbox-cli extension
deactivate hooks`). How much each agent can report depends on the
lifecycle surface its CLI exposes — claude, opencode, and antigravity
report the full range, codex reports idle/working/done, aider reports
blocked, and vibe is experimental. See the per-agent matrix in
`extensions/hooks/README.md` (and the website's *Agent hooks* page).

**Remote sessions report status too** (same per-agent range): at spawn
time the hook commands are rewritten to a tmux pane user option
(`@thurbox_state`) and each agent's hook config is shipped to the host —
claude's via its `--settings` arg, the config-dir agents via
`session_ops::remote_hooks` provisioning (probe → prune-then-merge or
managed-file write, best-effort). The local TUI receives changes over its
control-mode connection (a format subscription on tmux hosts; a 1 s
pane-option poller on psmux hosts, armed once the psmux gate —
`session::psmux_hook_rewrite_supported` — is flipped). With the TUI closed,
the headless `automation tick` (60 s heartbeat) polls hosts that have live
remote sessions and writes changed states into the same DB columns, so
remote status never freezes at its last pushed value. When wiring is
degraded (host
unreachable mid-provision, a user-owned file refused, or the
still-gated psmux provisioning), the session shows a `Hooks: degraded`
row in the info panel instead of silently idling. See the
*Remote SSH & WSL Sessions* section in `CLAUDE.md` for the full pipeline.

### Smart ordering & repo groups

The list is **grouped by repository** under subtle headers
(`── webapp ─────`), and within a group sessions follow their **manual
order** (`display_order`, see *Manual ordering* below). Manual order is
authoritative: once a row has been placed, a status change only
**recolors its dot**, it never moves the row. Sessions that were never
moved fall back to creation order:

- Sessions with no manual order render after ordered ones in stable
  insertion order. `Busy` and `Waiting` deliberately **share one
  "running" status**: a live agent flickers across the ~1s output
  boundary every tick, so they share a single dot colour rather than
  jittering between two. Ordering is a pure function of *manual order*
  and *stable order*, never of live timing — so the list never re-sorts
  itself, even when a session needs attention or exits.
- Groups are ordered by their **lowest member `display_order`**, then by name
  for determinism — so moving a session to the top of its group can pull the
  whole group up, but a status change never reshuffles the groups.
- The group key is the **set of repos a session spans** (order-independent), so
  a multi-repo session forms its own group with a combined header
  (`webapp + infra`) rather than being filed arbitrarily under one repo;
  sessions touching the same set cluster together. Sessions with no repo share a
  `(no repo)` group.

**Why group by repo?** With several parallel agents the dominant question
is "which project is this?" — clustering same-repo sessions answers it at a
glance, and a stable manual order means a row stays where you put it (a
blinking status dot still flags urgency without yanking the row around). A
single comparator (`ui::project_list::compute_session_order`) drives both
rendering and `Ctrl+J`/`Ctrl+K` navigation, so the keyboard always steps
through the exact order shown.

**Why signals instead of guessing?** Pure output-timing can only say
"quiet for >1s"; it can't tell a thinking pause from "done" or "needs
you". The agents already emit these signals — we just read them. This
mirrors how dashboards like Orca surface working / waiting / finished.

**Caveat (Claude in tmux):** Claude Code only emits the OSC 9 desktop
notification for Ghostty/Kitty/iTerm2, so inside thurbox's tmux pane
set `claude config set --global preferredNotifChannel terminal_bell`
to get the bell we can detect. We capture bell + OSC 9 + OSC 777,
whichever the agent produces.

### Manual ordering & alphabetical sort

The list is manually orderable. With the session list focused,
`Shift+J`/`Shift+K` move the selected session one row down/up
(rebindable `SessionListMoveDown`/`SessionListMoveUp`). A move swaps two
adjacent **blocks** — a row plus its nested children, so a parent drags
its whole subtree: root rows swap within their repo group, the **whole
group** swaps past a group edge, and nested children move among their
siblings only. `Shift+S` (rebindable `SessionListSortAlphabetically`)
sorts every group's sessions alphabetically by name in one shot,
preserving group order and parent/child nesting.

**With grouping off there are no group edges.** The session list's
`group_by_repo` setting (settings → Sessions) is not a label switch: off
means the list is genuinely one flat group ordered by `display_order`
alone, so `Shift+J`/`Shift+K` move a row one place anywhere in the list
and `Shift+S` sorts the whole thing. Suppressing only the header line
and keeping the clustering was the first shape and it was wrong — a move
that carried a session past a repo boundary was accepted, persisted, and
then undone by the next build re-clustering it under its own repo, with
the headers that would have explained it turned off. Parent/child
nesting is unaffected either way: it is not a repo property.

Both paths densely renumber every session's `display_order` `0..n` and
persist it, so the order survives restarts and syncs across instances
via the existing `data_version` polling. The pure helpers
(`ui::project_list::move_in_order` / `sort_alphabetically_within_groups`)
back `App::move_active_session` / `sort_sessions_alphabetically`; storage
is the nullable `sessions.display_order` column (schema v31, `None` =
never moved).

**Why manual order wins over status.** Earlier the list re-sorted itself
by status, which meant a row jumped around under your cursor every time
an agent finished or started thinking. Letting the user pin the order —
and only recoloring the status dot in place — keeps the list a stable
spatial map you can build muscle memory against.

---

## Session Creation

![Session creation workflow](../media/thurbox-session-creation.gif)

`Ctrl+N` walks through a series of modals to configure a new
session. Each step has a sensible default and can be skipped when
not applicable.

1. **Host picker** — choose where the session runs: `local`, or any
   remote SSH host defined in `hosts.toml`. Skipped entirely when no
   remote hosts are configured (preserving the local-only flow). For
   a remote host the repo picker shows the repos previously used *on
   that host* (bookmarks are host-scoped, schema v39) and every remote
   filesystem touch — the path browser's listings, Enter validation,
   `Alt+P` parent scans — runs on a worker, never blocking the UI on
   an ssh round trip; the worktree + tmux window are created on that
   host over SSH.
2. **Repo picker** — fuzzy-searchable list of bookmarked repo
   paths. `Space` toggles selection, `w` marks the selected repo
   as a worktree base (refused on a known non-git dir, which is
   still selectable as a plain member and rendered with a dim
   `(dir)` tag), `d` deletes the bookmark, and a path-input field
   adds new bookmarks: `Tab` accepts the inline autocomplete
   suggestion, or — with nothing to complete — opens a **path
   browser** dropdown listing the typed directory (git repos marked
   `●git`; `Enter` descends into a plain dir or picks a repo
   directly, `Esc` closes it, listings are cached per picker).
   Remote paths expand `~` against the remote home and are verified
   (exists + is-it-git, one round trip, async with a `checking…`
   spinner) on Enter; git-ness is persisted per bookmark (schema
   v40) so it's learned once. The first selected repo becomes the
   session's `cwd`; the rest may be exposed to the agent depending
   on the agent's own flags.
3. **Base branch selector** — worktree mode only.
4. **Session name** — free text identifier shown in the sidebar.
5. **New branch name** — worktree mode only.
6. **Agent picker** — choose which coding agent runs in this
   session. Skipped when only one agent is defined in
   `agents.toml`.

**Creating a session moves nothing — unless you ask it to.** By default the new
row appears in the list and waits to be picked; the selection, the pane showing
it and the keyboard all stay where they were. Creation is a command that
finishes on a worker seconds after the flow closed, so the moment it lands is
not a moment the user chose — steering the view then interrupted whatever they
had gone back to reading, and made creating three sessions in a row a fight with
the cursor. `Ctrl+F` fork behaves the same way. Selection is still *steerable*,
by the two requests that are deliberate: a clicked notification and
`thurbox-cli session focus`, both through `focus_session`, which the list
follows by id rather than by row number.

Not having to hunt for the row you just asked for is worth that interruption to
some people, so it is **a setting rather than a decision**: the session list's
`focus_new_session` (settings → Sessions, off by default) makes a create or a
fork select the new session and give the agent pane the keyboard, exactly as
`Enter` on its row would. It is the *list's* setting rather than a core one
because the list owns the selection — it subscribes to `session.post_create`
and does there what `Enter` does. That event fires only for a create **this
interface** performed, which is what keeps a `thurbox-cli session create`, an
automation or a second instance from taking the keyboard out from under you;
and the cursor only *follows* the new id, so moving it yourself in the meantime
wins.

A session is fully described by its repos and agent. There is no
per-session model selection, permissions, prompt, tool, or skill
configuration — those concerns belong to the agent CLI itself,
which runs with its own default config.

**Why per-session repo selection?** Each session is its own context,
so it makes sense to pick repos at creation time rather than
inheriting from a parent grouping. Mixed sessions are supported:
some repos may be worktree-based (new branch created) while others
are added as-is.

**How does one agent reach multiple repos?** Agent CLIs disagree on
how (or whether) to accept extra directories, so thurbox stays
agent-neutral: a multi-repo session is launched in a per-session
**symlink workspace** (`~/.local/share/thurbox/workspaces/<id>/`)
holding one symlink per repo, with the agent's cwd set there. Every
agent then sees each repo as a subdirectory — no per-agent flags and
no `agents.toml` changes. The workspace is only symlinks, rebuilt
idempotently on each launch and removed (without touching the repos)
when the session is deleted. Single-repo sessions launch directly in
the repo as before.

**Headless multi-repo.** The same shape is reachable without the TUI.
`thurbox-cli session create` (and `task create`) take repeatable
`--add-repo PATH[@BASE]` — each gets its **own isolated worktree** on
the spawn's shared `--worktree-branch`, off its own base — and `--add-dir
PATH`, which attaches a repo **as-is** (no branch). A spawn with two or
more members lands in the same symlink workspace the TUI builds, so every
agent sees each repo as a subdirectory. The extra-repo list is persisted
as JSON (schema v33) so a restored session rebuilds the identical
workspace.

**Why per-session agent?** Different tasks suit different agents.
Choosing the agent at creation time keeps each session
self-describing and lets you mix agents across the sidebar
(Claude here, Codex there) with no shared global configuration.

**Why a bookmark list rather than a path picker every time?** Users
work on the same handful of repos repeatedly. Bookmarks make the
common case a 2-keystroke selection while still allowing arbitrary
paths via the input field. Bookmark deletion (`d`) keeps the list
from accumulating stale entries.

### Agent definitions

The set of available agents is **data**, not code. On first run
Thurbox seeds `~/.config/thurbox/agents.toml` with built-in
definitions for claude, codex, antigravity, opencode, aider, copilot,
vibe, and pi (`agent::agent_config::load_or_seed`). Editing the file —
adding an `[[agents]]` entry or tweaking an existing one — extends the
agent picker with no recompile. Each built-in's exact config and
behavior (and the checklist for adding a new built-in) is in
[AGENTS.md](AGENTS.md).

Each definition (`session::AgentDef`) carries:

- `name` — display + lookup key, unique in the registry.
- `command` — the CLI executable to launch.
- argument-template groups: `args` (always passed — bake in flags
  like a model here if you want) and `resume_args` / `fork_args` /
  `new_session_args` (with `{id}`).
- `resume_latest` — when true, restart resumes the agent's most
  recent session in the launch directory via **id-less** flags
  (see below).

`agent::GenericProvider` builds the launch arguments by appending
each group **only when its driving value is present**, substituting
`{id}` token-by-token. Selection precedence is fork > resume >
new-session id; static `args` follow. A group with no value is
simply omitted — no unresolved-placeholder heuristics.

Only `claude` and `pi` accept the thurbox-generated id at creation
(`--session-id {id}`), so only they resume/fork by that exact id.
The other built-ins can't pin or report their session id, so they
set `resume_latest = true` and resume/fork via id-less, cwd-scoped
flags (`codex resume --last`, `opencode --continue`, `agy
--continue`, `aider --restore-chat-history`); the agent
resolves "the last session in this directory" itself, which works
because restart reuses the session cwd and a single-repo fork reuses
the parent cwd. Agents that declare no `resume_args` start fresh on
restart instead of resuming.

Example:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]

[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]   # id-less: last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

### Remote SSH & WSL sessions

Like agents, off-local hosts are **data**. A session can run on a
remote machine over SSH, or inside a local **WSL distro**, while the
TUI stays local. Hosts are declared in
`~/.config/thurbox/hosts.toml` (seeded commented-out, so a fresh
install has none and behaves exactly as before) — **and WSL distros
are auto-discovered on Windows** (`wsl.exe -l -q`), so they need no
entry at all:

```toml
[[hosts]]
name = "devbox"            # selectable as backend "ssh:devbox"
destination = "me@devbox"  # resolved via ~/.ssh/config
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m"]

# Only needed to override an auto-discovered WSL distro's defaults:
[[hosts]]
name = "ubuntu"            # selectable as backend "wsl:ubuntu"
kind = "wsl"
distro = "Ubuntu-22.04"    # defaults to `name`
```

Each `[[hosts]]` entry (`session::HostDef`) — the seeded `hosts.toml`
documents each field inline:

| Field | Required | Default | Meaning |
|-------|----------|---------|---------|
| `name` | yes | — | unique id; registers the backend `ssh:<name>` / `wsl:<name>` and is what `--host` expects |
| `kind` | no | `ssh` | transport: `ssh` (remote machine) or `wsl` (local distro) |
| `destination` | for ssh | — | ssh target (`user@host` or a `~/.ssh/config` alias) |
| `distro` | no | `name` | WSL distro name (`kind = "wsl"` only) |
| `ssh_opts` | no | `[]` | extra `ssh` flags, one token per array element; no `~` expansion (use absolute paths) |
| `socket` | no | `thurbox` | host `tmux -L` socket name |
| `session` | no | `thurbox` | host tmux session name |
| `worktrees_dir` | no | `$HOME/.local/share/thurbox/worktrees` | absolute dir on the host/distro for git worktrees |
| `share_sessions` | no | `true` | the host's database is the record of its sessions (see **Shared sessions** below); `false` drives the host from here as before |

Each host becomes a session backend named `ssh:<name>` / `wsl:<name>`.
For **SSH**, thurbox shells out to the system `ssh` binary, so
authentication, keys, and connection multiplexing come from your
`~/.ssh/config` — thurbox never handles credentials. A **WSL distro**
is reached with `wsl.exe -d <distro>` (no credentials, no network);
`wsl.exe` forwards whitespace-free tokens to the in-distro shell like
`ssh` does, so the *same* tmux control-mode protocol, POSIX quoting,
and worktree layout apply — only the launch prefix differs (multi-word
`sh -c` scripts go through `wsl.exe --exec`, which hands argv over
verbatim; see `shell::wsl_command`). So off-local
sessions get identical persistence, multi-instance sharing, and
restore-on-startup as local ones; the worktree and agent process live
on the remote host / inside the distro (a WSL distro's worktrees stay
in its own Linux filesystem, not on `/mnt/c`). In the session list an
off-local session is marked with a `☁` glyph (and the info panel shows
its `Host:`), mirroring the worktree `⑂` mark.

**Why a config file rather than ad-hoc destinations?** Named hosts
give the picker stable, readable entries and let `backend_type`
round-trip cleanly through the database so a remote session re-adopts
on the correct host after a restart.

**Why lean on `~/.ssh/config`?** Re-implementing SSH auth, agent
forwarding, and ControlMaster multiplexing would be a large, fragile
surface. Deferring to the system `ssh` keeps thurbox out of the
credential path and inherits whatever the user already configured.

Headless: `thurbox-cli session create --host devbox --repo-path
/srv/repo --worktree-branch feat/x` does the same over the CLI.

#### Shared sessions: the host's database is the record

A session that runs on a host is a row in **that host's** thurbox
database, whoever created it — a thurbox running on the host and one
reaching it as `ssh:<name>` see the same list, and either side can
create, delete, restart or restore. ADR-24 in `docs/ARCHITECTURE.md`
has the rationale; the shape:

- **Mirror.** A remote thurbox mirrors the host's `session list
  --json` (and `--deleted`) into local rows on `ssh:<name>` — same id,
  the host's facts and hook status — every 10 s from a worker, right
  after anything it delegated, and from the headless `automation tick`.
  `thurbox-cli session sync [--host <name>]` runs one pass by hand.
  What is the observer's stays the observer's: display order, the
  companion shell. A pass that changes nothing writes nothing.
- **Delegation.** Create, delete (soft or forced), restart and restore
  on a shareable host run `thurbox-cli session …` *on the host*, which
  does the worktree, the hooks and the launch with the host's own
  `agents.toml` and `hooks.toml` and mints the id. Every caller goes
  through the same four `session_ops` pipelines, so the creation flow,
  the CLI, `spawn` automations and extension self-heal all delegate.
  The caller's own `hooks.toml` fires around the delegated call with
  `THURBOX_HOST` set; the host's fires there. A refusal on the host is
  the caller's error, verbatim.
- **Provisioning.** On first use, thurbox looks for a `thurbox-cli` of
  the same major — first the one a thurbox running *on the host*
  advertises (every thurbox links its own CLI at
  `<data dir>/bin/thurbox-cli` at start and on each CLI call, which is
  what makes a host running a **dev checkout** shareable at all: its
  `target/debug/thurbox-cli` is on nobody's PATH), then PATH and
  `~/.local/bin`. When
  there is none, it downloads the release archive of **its own
  version** for the host's platform, verifies it against the release
  checksums, and places `thurbox-cli` under
  `~/.local/share/thurbox/bin/` on the host (`thurbox-dev/bin/` for a
  dev build, which then uses the host's `thurbox-dev` database and
  socket — dev and release stay as separate there as they are locally)
  — never on PATH; an install the user makes later wins. That first
  session creates the
  host's database at the standard location, so a later full install
  finds every session already there. A dev build ships its own sibling
  binary when the host is the same platform and refuses otherwise.
- **When it cannot.** No CLI and no artifact (a dev build on a foreign
  platform, no network, a schema mismatch), or `share_sessions =
  false`: the host is used exactly as before — worktree over ssh, the
  hooks rewrite, the pane-option status channel — and `session create`
  says so (`sharing` in its JSON, a line in `thurbox.log`). Sessions
  created that way are listed by `session sync` as unknown to the host;
  `session sync --host <name> --adopt` registers them there.
- **Retrying.** A host that answers "no usable CLI" is asked again
  after 60 s, and each further consecutive failure doubles that up to
  15 minutes — so a host that is merely rebooting is picked up on the
  next pass, while one that can never be provisioned stops costing an
  archive download and a connection every minute. The first usable
  answer resets it, and so does `session sync`, since running it by
  hand usually means the host was just fixed.
- **Status.** Hooks on a shared host call the host's own `thurbox-cli
  session signal`, which writes the host's database (mirrored at 10 s)
  **and** the pane option a remote observer's control-mode subscription
  already reads, so a tmux host's status still lands within a second.
  On a Windows (psmux) host status arrives through the mirror — which
  replaces the `Hooks: degraded` those hosts showed.
- **Reboot.** The host relaunches its own sessions, as it does locally.
  A remote observer whose survey finds a mirrored row with no window
  asks the host to relaunch it (`session restart --if-missing`), and
  the host launches only if the window is still absent — so two
  observers asking produce one launch. A window killed by hand is
  indistinguishable from a crash and comes back the same way.
- **Undo and restore.** `Ctrl+Z` inside the undo window leaves no
  trace. Once the host records a deletion every mirror shows it;
  a restore from any side runs on the host and every mirror shows it
  back. A soft delete asked for headlessly is reaped by the host's tick
  once the undo window has passed.
- **Windows hosts** share through the same path: the probe, the
  provisioning (the release zip) and every delegated command go
  through the PowerShell path the probes already use.
- **Two observers on one pane** resize it to their own rects — the
  existing behaviour for two thurbox instances on one database.

---

## Keybinding Design

### Philosophy: Ctrl = global, everything else = PTY

When the terminal panel is focused, **all keys are forwarded to the
PTY** except those with a `Ctrl` modifier (intercepted as global
commands) and `Shift+arrow/page` / `Alt+page` keys (intercepted for
scrollback).

**Why Ctrl, not Alt?**

- Coding-agent CLIs and shell programs heavily use Alt-key
  combinations. Intercepting Alt would break readline, vim, and the
  agent's own keybindings.
- Ctrl has well-established precedent for "meta" actions in
  terminal multiplexers (tmux uses `Ctrl+B`, screen uses `Ctrl+A`).
- Ctrl combos are easier to type one-handed, which matters for a
  tool used alongside other terminals.

### Keybinding Table

All global keybindings use `Ctrl` and follow Vim conventions where
applicable: `h/j/k/l` for navigation, semantic letters for actions
(`D`=delete, `N`=new, `R`=restart, `Q`=quit).

| Key | Context | Action | Mnemonic |
|-----|---------|--------|----------|
| `Ctrl+Q` | Global | Quit Thurbox (detach sessions) | **Q**uit |
| `Ctrl+N` | Global | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Terminal | Copy selection, or send SIGINT if none | **C**opy |
| `Ctrl+V` | Terminal | Paste from clipboard into PTY | Paste |
| `Ctrl+P` | Global | Command palette — every action, filtered as you type | **P**alette |
| `Ctrl+W` / `F5` | Global | Toggle tasks panel (todo list) | Work items |
| `Ctrl+/` | Global | Global search across every scope | **/** = search |
| `Ctrl+T` / `F8` | Global | Toggle shell pane alongside the agent session | **T**erminal |
| `Ctrl+X` / `F7` | Global | Toggle the native code-review view | Review |
| `Ctrl+H` | Global | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Global | Select next session | Vim: **j** = down |
| `Ctrl+K` | Global | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Global | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Session list | Delete selected session | Vim: **d** = delete |
| `Ctrl+O` | Global | Open active session's worktrees in editor | **O**pen |
| `Ctrl+R` | Global | Restart active session | **R**estart |
| `Ctrl+F` | Global | Fork active session | **F**ork |
| `Ctrl+S` | Global | Sync all worktree sessions with their base branch | **S**ync |
| `Ctrl+Z` | Global | Undo session delete | **Z** = undo |
| `Ctrl+U` | Global | Restore deleted sessions list | **U**ndelete |
| `Ctrl+Y` / `F4` | Global | Pick TUI theme | Color **Y**oke |
| `Ctrl+,` / `F6` | Global | Settings panel (edit settings.toml) | **,** = preferences |
| `F1` / `Ctrl+G` | Global | Keybindings help + interactive editor | Universal help |
| `Ctrl+B` / `F2` | Global | Toggle info panel | **B**rowse info |
| `Ctrl+E` / `F3` | Global | Toggle file viewer | **E**xplore files |
| `Shift+J` | Session list | Move selected session down | Reorder |
| `Shift+K` | Session list | Move selected session up | Reorder |
| `Shift+S` | Session list | Sort sessions alphabetically within repo groups | **S**ort |
| `j` / `k` | F1 editor | Select action to rebind | |
| `Enter` / `r` | F1 editor | Capture a new chord for the selected action | **R**ebind |
| `d` | F1 editor | Reset selected action to its default chord(s) | **D**efault |
| `Shift+D` | F1 editor | Reset all actions to their defaults | Reset all |
| `Esc` | F1 editor | Close (or cancel an in-progress capture) | |
| `j` / `Down` | Lists | Next item | |
| `k` / `Up` | Lists | Previous item | |
| `Enter` | Global search | Jump to selected result | |
| `Esc` | Global search | Close search | |
| `Enter` | Session list | Focus terminal | |
| `j` / `Down` | Repo picker | Next repo | |
| `k` / `Up` | Repo picker | Previous repo | |
| `Space` | Repo picker | Toggle repo selection | |
| `w` | Repo picker | Toggle worktree mode for repo | |
| `d` | Repo picker | Delete bookmark | |
| `Tab` | Repo picker | Switch to path input | |
| `Tab` | Repo picker input | Accept suggestion, else open the path browser | |
| `↑`/`↓` | Repo picker browser | Move the dropdown selection | |
| `Enter` | Repo picker browser | Descend into a dir / pick a git repo | |
| `Esc` | Repo picker browser | Close the dropdown (modal stays open) | |
| `Alt+P` | Repo picker | Import typed path as a parent folder (local + remote) | |
| `Enter` | Repo picker | Confirm selection | |
| `Esc` | Repo picker | Cancel | |
| `Shift+Up` | Focused terminal | Scroll up 1 line | |
| `Shift+Down` | Focused terminal | Scroll down 1 line | |
| `Shift+PageUp` / `Alt+PageUp` | Focused terminal | Scroll up half page | |
| `Shift+PageDown` / `Alt+PageDown` | Focused terminal | Scroll down half page | |
| Mouse wheel | Focused terminal | Scroll up/down 3 lines | |
| Click | Session/task/automation/file row | Select the row and focus its pane | |
| Click | Any pane | Focus the pane under the cursor | |
| Click | Picker modal row | Select and confirm (Enter; repo picker: Space toggle) | |
| Hover | Clickable rows | Underline the row a click would hit | |
| All other keys | Focused terminal | Forwarded to PTY (snaps to bottom if scrolled) | |

### Customizing shortcuts

Nearly every shortcut can be remapped, including copy/paste, file-viewer
navigation, session-list navigation, and terminal scroll. The F1 panel doubles
as a live editor: select an action with `j`/`k`, press `Enter`/`r`, then press
the chord you want — the next physical keypress (including chords like
`Ctrl+Q`) becomes that action's sole binding. `d` restores the selected
action's defaults, and `Shift+D` resets every action at once (removing the
override file). If the chord conflicts it is reassigned from the other action
and a status toast reports the move. Changes persist immediately to
`~/.config/thurbox/keybindings.json` (`Action` name → chord strings, e.g.
`{ "QuitApp": ["ctrl+a"] }`) and take effect on the next keystroke — no
restart. The file can also be hand-edited directly.

**Context-scoped keys.** Each action belongs to a scope — `Global`,
`SessionList`, `Automations`, `Tasks`, `FileViewer`, or `Terminal`. Global
actions fire anywhere; scoped actions fire only while their pane is focused, so
the same single-letter key (e.g. `j`) can drive the file viewer, session list,
automations pane, and tasks pane independently while the terminal still forwards
it to the shell. Conflicts are only flagged between actions whose scopes
overlap. A handful of stateful keys stay fixed (shown in the F1 panel under
*Fixed (not rebindable)*): modal selectors (`j`/`k`/`Enter`/`Esc`), the
automation run-history sub-mode, the file-viewer search sub-mode, and the
terminal's catch-all PTY forwarding.

**Readline editing in modal text fields.** Thurbox's own text inputs
(session / branch name, repo-picker path & search, automation editor,
task title / description) accept the standard emacs/readline
line-editing chords, so the muscle memory that works in a terminal works
there too: `Ctrl+A`/`Ctrl+E` (line start/end), `Ctrl+B`/`Ctrl+F` (move
by char), `Ctrl+H`/`Ctrl+D` (delete before/under the cursor),
`Ctrl+W` (delete word), `Ctrl+U`/`Ctrl+K` (kill to line start/end). The
dispatch lives in one place (`modals::apply_ctrl_line_edit` over the
`LineEdit` trait), and **every** `Ctrl`+letter is consumed (mapped or
swallowed) so a bare control letter never leaks into the field.

### macOS

Ctrl chords pass through macOS terminals unchanged (raw mode disables
flow control; the `Ctrl+Y` DSUSP quirk is why the `F4` alternate
exists). Beyond that:

- **Cmd as a modifier.** Thurbox enables the kitty keyboard protocol
  when the terminal supports it, so the Command key is a first-class
  modifier: rebind an action onto `cmd+j` from the F1 editor (`super`,
  `command`, and `win` parse as aliases; `cmd` is canonical). Supported
  by iTerm2 3.5+, kitty, WezTerm, and Ghostty; Terminal.app lacks the
  protocol, so Cmd chords never arrive there (everything else degrades
  gracefully). Note the emulator consumes its own Cmd shortcuts
  (`Cmd+Q/W/N/T/C/V`, `Cmd+K` clear, `Cmd+H` hide, `Cmd+digit` tabs)
  before Thurbox can see them — only unclaimed chords are bindable.
  The modifier reaches the registry at all only since issue #1024: it
  was dropped when a keypress was flattened, so `Cmd+C` arrived as a
  bare `c` and every `cmd+…` binding was unreachable.
- **macOS default alternates.** One pair, appended after the Ctrl
  primaries on macOS builds (Linux defaults are otherwise identical):
  `Cmd+C` / `Cmd+V` copy and paste, because `Ctrl+C` in a terminal is
  the interrupt. The pattern is "Cmd mirrors the Ctrl primary". These
  are two of the chords an emulator commonly claims for itself, so
  whether they arrive is the emulator's decision — see *Text Selection
  and Copy-Paste* below. v1 also shipped `Cmd+J`/`Cmd+L` alternates for
  session and pane movement; they went with v1's key table, and pane
  focus has no binding to alternate — it is a reserved chord
  (`Ctrl+H`/`Ctrl+L`).
- **Unbound Cmd chords are swallowed**, never forwarded to the PTY:
  injecting the bare letter into the agent would corrupt its input.
- **F-keys** (`F1`–`F5` alternates) require `Fn` on Mac laptops
  unless function keys are set to standard; `Cmd+V` already pastes
  through the terminal's native paste → bracketed paste path.

### Windows

- **AltGr is not a chord.** The Windows console reports an AltGr press
  as left-`Ctrl` plus right-`Alt`, so every character a layout hides
  behind AltGr (`\` and `|` on AZERTY; `@`, `[`, `]`, `{`, `}` and `~`
  on QWERTZ) arrives carrying two modifiers. Thurbox drops that pair
  before anything looks at the keystroke
  (`coordinator::input::resolve_altgr`), so the character is typed into
  a field and sent to the agent as itself rather than being swallowed as
  an unbound chord or wrapped in an `ESC`. The pair is dropped only for
  a character no key produces unmodified — punctuation, or a non-ASCII
  letter (`ą`, `€`) — so a real `Ctrl+Alt`+letter/digit chord still
  resolves as one. Off Windows, AltGr is a level-3 shift the terminal
  composes before thurbox sees it, and `Ctrl+Alt`+punctuation stays
  bindable.

---

## Session Lifecycle

```text
Create (UUID v4) → Running → Idle / Error
                      ↓
                  Shutdown (SIGHUP)
```

### States

- **Running**: PTY is alive, read loop is active, output is
  streaming to the terminal widget.
- **Idle**: the agent CLI has exited cleanly (exit code 0). Session
  is still displayed but no longer accepts input.
- **Error**: PTY or the agent CLI exited with a non-zero code. Error
  details shown in status bar.
- **Shutdown**: Triggered by the user closing a session or quitting
  the app. Sends `SIGHUP` to the PTY child process, then waits for
  clean exit before dropping resources.

### Session Restart (`Ctrl+R`)

Restarts the active session's tmux pane while preserving the
conversation history. The session is killed and respawned with the
agent's resume arguments (e.g. `--resume <id>` for Claude, or
id-less `resume --last` for a `resume_latest` agent like codex),
reusing the session's stored agent. Agents that define no
`resume_args` simply start a fresh conversation.

**Why restart instead of close + new?**

- Closing destroys the agent's session ID. Restarting uses the
  agent's resume arguments so the conversation context is
  preserved (when the agent supports it).
- The session's `SessionInfo` (ID, name, agent, repos)
  stays intact — only the backend pane and I/O are replaced.

### Session lifecycle hooks (`hooks.toml`)

The user's own commands, run around the four session operations: before
and after a session is created, deleted, restarted or restored. Declared
as data (`~/.config/thurbox/hooks.toml`, one `[[hooks]]` entry per
event + command), seeded commented-out, read each time an event fires.

The mechanism is where they fire, not what they are. Every interface —
the TUI's flow and chords, `thurbox-cli`, a `spawn` automation, an
extension's self-healed sessions — ends in the same four functions in
`session_ops` (`spawn`, `delete`, `restart`, `restore`), so a hook placed
inside those fires **once per operation for every caller**, and nothing
in the kernel or the Lua interface knows hooks exist. They run on the
thread that runs the operation — a worker in the TUI — never the render
loop.

**Pre hooks veto, post hooks inform** — git's `pre-commit` model. A
`pre_*` that exits non-zero (or hangs past its timeout, default 30 s) aborts
the operation before its first side effect, with its stderr tail as the
reported reason; a `post_*` fires only after full success, every one
runs, and a failure is logged and reported (`hook_failures` in the CLI
JSON) but cannot undo what happened.

A hook receives the session's facts as `THURBOX_*` environment variables
and as one JSON object on stdin, inherits the config/data-dir overrides
so a `thurbox-cli` it runs hits the right database, runs in the primary
repository (the one path that exists at every event) with no terminal,
and — for a remote session — runs *locally*, told the host by
`THURBOX_HOST`. Full reference: `docs/CONFIG.md` → hooks.toml.

Deliberately not the same thing as the built-in `hooks` *extension*
(`<config>/hooks/`), which is the reverse direction: files thurbox
installs into the agent CLIs so they can report status.

The four `post_*` events are also delivered to interface plugins under the
same names (`events = { "session.post_create" }` + `on_event`, see
`docs/PLUGINS.md` → Events), so a shell hook and a Lua handler learn one
vocabulary. A `pre_*` hook has no Lua form: a plugin cannot answer, so it
cannot veto.

### Why UUID v4?

Sessions need unique identifiers for the lifetime of the process.
UUIDs are collision-free without coordination, simple to generate,
and usable as map keys. Sequential IDs would work too, but UUIDs
prevent bugs where an old session ID accidentally refers to a new
session after recycling.

---

## Editor Integration (`Ctrl+O`)

`Ctrl+O` opens the active session's working directories in a
configured external editor. The editor command is a global setting
stored in SQLite, defaulting to a sensible value on first run.

**Terminal editors are first-class.** A terminal editor (vim, nano,
`ttt`, helix, micro, …) needs a controlling TTY, which the old
fire-and-forget detached spawn did not provide. So `Ctrl+O` now runs
terminal editors with a real TTY: when thurbox is **inside tmux** the
editor floats in a `tmux display-popup` (the TUI keeps running
underneath, the popup closes on editor exit), and when it is **not**
the TUI is suspended and the editor inherits the terminal (the
git/sudoedit pattern — the TUI resumes on editor exit). GUI editors
(`code`, `zed`, …) keep spawning detached as before, so they still pop
their own window while the TUI stays interactive.

**Auto detection + override.** In the default `auto` mode the launch
path is chosen from the command name (curated terminal/GUI lists;
`emacs -nw` and `--tty`-style flags force the terminal path). Force it
explicitly with `thurbox-cli editor mode terminal` (TTY path for every
editor) or `gui` (detached spawn for every editor — the pre-terminal
behavior).

**Why a configurable command rather than just `$EDITOR`?** A separate
setting lets users point at `code`, `cursor`, `idea`, etc. without
disrupting their shell environment; `$VISUAL`/`$EDITOR` are still
honored as the fallback when no command is set.

**Why all worktrees, not just cwd?** Multi-repo sessions touch
several directories at once; opening only the cwd would hide the
rest. The editor command receives every working path so the user's
editor of choice can open them as a workspace.

---

## Code Review (native)

> **Not in the binary.** The view was deleted with `src/ui`; the data layer
> survived and a plugin took it up:
> [`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review), the
> first consumer of `thurbox.diffs` anywhere, which reclaims `Ctrl+X` / `F7` and
> installs with
> `thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-code-review`.
> What it builds on is still here: `session::review` (pure diff types +
> `parse_unified_diff`), `storage::review` (`review_comments` + `review_marks`,
> schema v38), and `kernel::diff` (diffs on a worker, published into the snapshot).
> v1 keeps the view on `v1.x`.

Thurbox ships a **native, built-in** tuicr-like review view (`Ctrl+X`, `F7` alternate): a
GitHub-style continuous diff of the active session's worktree
(`<base>..HEAD`) with classified comments (issue / suggestion / note /
praise), per-file/hunk "reviewed" marks, and a review summary — rendered
directly by thurbox and persisted in SQLite.

**Why native, not the external `tuicr` binary?** An earlier attempt
launched `tuicr` inside a tmux pane. Nesting a full ratatui TUI inside
thurbox's vt100 parser is janky (double-render, input quirks), needs the
binary installed, and the feedback loop was clunky. Rendering the diff
ourselves makes it a first-class panel: instant toggle, real mouse
support, and direct access to the session's git state and agent.

**Why a central-pane view with its own focus (not a `TerminalView` like
the shell)?** The shell pane forwards keystrokes to a PTY; a review view
must *capture* keys (navigation, commenting). So it gets its own
`InputFocus::CodeReview` and owns the central pane while open, modeled on
the file-viewer/task panels rather than the shell toggle.

**Why a changed-files list in the file-viewer column?** A large diff is
hard to navigate as one stream, so the file-viewer column lists the
changed files (forced visible while a review is open); it tracks the file
under the cursor and clicking a row jumps the diff to that file. The
diff stays a single continuous stream (closest to tuicr) — the list is a
jump aid, not a separate per-file view. `{`/`}` jump files and `[`/`]`
jump hunks, matching tuicr.

**Why selectable review targets?** Like tuicr (`-r`/`-w`/a commit), the
diff can show the whole branch (`<base>..HEAD`), the uncommitted working
changes (`git diff HEAD`), or a single commit (`git show`). `t` opens an
in-view picker listing Working, Branch, and each commit in the range;
selecting one recomputes the diff. A session with no resolvable base
defaults to the working-changes target, so even a bare checkout reviews.

**Why review all repos at once?** A thurbox session can span several
repositories (and flow opens a PR per repo), so a review that only saw the
primary repo would miss most of the change. A multi-repo session reviews
every worktree in one stream: each repo's diff is built and concatenated,
with file paths namespaced `<repo>/<path>` so files, comments, and
reviewed-marks never collide across repos. Each repo resolves its own base
(the session base if that branch exists there, else its own default
branch); the commit target lists commits across all repos, repo-tagged.

**Why unified *and* side-by-side?** tuicr offers both (its `diff_view`);
`v` toggles them. The side-by-side layout is **true paired** — a deletion
(left) and its aligned addition (right) sit on the *same* screen row
(positional `del[k] ↔ add[k]` alignment, `session::review::pair_hunk`),
so a modified block reads as N rows instead of the 2N a stacked layout
takes. The core invariant is preserved: a paired row is still **one
selectable unit** (the pairing is a rendering concern; `ReviewRow::Line`
stays row-granular), and which side a comment attaches to is resolved at
compose time — keyboard defaults to New (the addition), a mouse click uses
the column it hit (left = Old, right = New). Alignment is positional
(dependency-free, matching the heuristic syntax highlighter); token-level
intra-line word diffs, and horizontal-scroll/wrap parity in the paired
layout, are follow-ups.

**Why syntax highlighting?** Plain diffs are hard to skim. A small,
dependency-free lexer (`ui::syntax`) colours comments / strings / numbers
/ keywords / type names from the theme palette, so code reads like code.
Add/remove stays on the gutter `+`/`-` and the row tint, leaving the text
free to carry syntax colour. It's heuristic + language-agnostic (no
grammar engine, no heavy dependency); a grammar-aware upgrade is a
follow-up.

**Why mouse-first, no vim modal?** To match thurbox's own interaction
model (clicks, buttons, scrollbars, wheel) rather than tuicr's heavy vim
modes — though the tuicr movement keys (`j`/`k`, `{`/`}`, `[`/`]`,
`g`/`G`) work too. A comment is composed in an in-view box that **floats
inline at the line** being commented (not pinned to the bottom), so the
edit happens where you're looking; "mark reviewed" works from any row in
the file, not just its header.

**Why persist a base branch?** Reviewing `<base>..HEAD` needs the fork
point, which thurbox didn't store. A write-once `sessions.base_branch`
column (schema v38, like the hook columns) records it at spawn; legacy
rows fall back to the repo's default branch.

**Why a folder tree + fold-on-reviewed?** A flat changed-files list
buries structure in a large diff, so the file-viewer column renders the
changes as a **folder tree** (directories as headers, files indented,
grouped by path; multi-repo nests the repo as the top folder) with
colored status glyphs (`M`/`A`/`D`/`R`) and `+`/`-` counts. Marking a
file reviewed (`r`) **folds** its diff to just the header — tree-style —
so reviewed code collapses out of the way; `Enter` expands/collapses any
file manually (`is_file_folded` = `reviewed XOR fold_override`).

**Why keep reviews open per session?** A review is per-session state
(`App::code_reviews`, keyed by `SessionId`), exactly like the shell
view: switching to another session hides it and switching back restores
it open + focused (`sync_review_focus` keeps the central-pane focus
aligned). The file-viewer column toggles with it.

**Export is the agent, not GitHub.** GitHub/GitLab submit is out of
scope; the payoff of reviewing *inside* an orchestrator is closing the
loop — `Send→Agent` pastes the compiled review into the session's agent
to address, and `Copy` yields markdown. Diff data types + the unified-diff
parser live in `session::review` (pure, so `ui` renders them without
importing `git`); persistence in `storage::review`.

### Implementation reference: surface, layout, and helpers

The full surface — every key, the layout invariants, and the named helper behind
each part. `CLAUDE.md` keeps a summary and points here.

- **Surface (tuicr-like).** A continuous diff stream in the central pane (its own
  `InputFocus::CodeReview` — unlike the shell pane's `TerminalView`, it *captures*
  keys), plus a **changed-files list in the file-viewer column** (forced visible
  via `layout_for`) that tracks the current file; clicking a row jumps the diff to
  it (`ui::code_review::render_files_list` → `ClickAction::ReviewFile` →
  `cr_jump_to_file`). That list is itself a **focusable pane**
  (`InputFocus::ReviewFiles`, the ring stop replacing `FileViewer` while a review
  owns the column): `j`/`k` (+ arrows) walk file→file with the diff following,
  `g`/`G` first/last *file*, `Ctrl+D`/`U` + PageUp/Down half-page, `Enter`/`l`
  drop into the diff at that file, `r`/`R` toggle the file/hunk reviewed mark,
  `Esc` closes the review (`App::handle_review_files_key`, captured before the
  global lookup like the diff pane). `Esc`/`Ctrl+X` (or `F7`) close the view.
  Rendered by `ui::code_review`, reusing `scrollbar`/`focus_block`/
  `render_button_bar`/theme. **Unified or true paired side-by-side** layout,
  toggled with `v` / the footer button (`side_by_side`): a deletion (left) and its
  aligned addition (right) share **one** screen row (positional `del[k] ↔ add[k]`
  via the pure `session::review::pair_hunk`; unpaired remainders get a blank
  half-cell). Pairing is rendering-only — `ReviewRow::Line` stays row-granular, so
  a paired row is one selectable unit and every `match` on it is unchanged;
  `push_file_rows` just emits one row per pair. Which side a comment attaches to
  resolves at compose time (`CodeReviewState::selected_anchor`): keyboard defaults
  to New, a mouse click uses the column it hit (`App::cr_click_row` →
  `click_side`; left = Old, right = New). **Mouse-first** (no vim modal): click a
  line to select/comment, click footer buttons, drag the scrollbar, wheel-scroll.
  **tuicr nav keys**: `j`/`k` + arrows, PageUp/Down + `Ctrl+D`/`U`, `g`/`G`,
  `{`/`}` (or Tab) next/prev file, `[`/`]` next/prev hunk. Every footer button is
  labelled with its key (`Comment·c`, `Send→Agent·e`, `Find·/`, …); the
  changed-files column shows a nav-key legend.
- **Long lines: horizontal scroll + wrap toggle.** By default the body scrolls
  horizontally with `Left`/`Right` (or `h`/`l`) while the line-number gutter stays
  pinned (`CodeReviewState::h_scroll`, stepped by `App::cr_scroll_h`, clamped to
  the longest line). A **wrap toggle** (`w` / the `Wrap`/`NoWrap` footer pill,
  `CodeReviewState::wrap`, `App::cr_toggle_wrap`) soft-wraps instead. **Wrap works
  in both layouts** — a paired row wraps each half independently and the taller
  half drives the visual-row count (the shorter pads blank); horizontal scroll
  stays unified-only (side-by-side pins `h_scroll = 0`). The invariant **1 logical
  diff row = 1 selectable unit** holds: selection, comment anchoring, click
  hitboxes, and the `selected`-primary scrollbar stay logical, while wrapping only
  expands the *visual* rows in `render_rows` and every sub-row carries its parent's
  logical index (a click on a continuation selects the whole line; compose anchors
  to the first visual row). Rendering: `unified_diff_line` (h-scroll) /
  `unified_diff_line_wrapped` / `paired_diff_line` (wrap-aware), with row counts
  mirrored by `visual_line_count` / `paired_visual_count` for the scroll walk.
- **Find in diff (`/`).** A `/`-triggered find sub-mode (also the `Find·/` button,
  and `/` from the changed-files pane) searches every visible row's text — file
  paths, hunk headings, diff line bodies, comment bodies (case-insensitive literal
  substring) — via the pure `CodeReviewState::{row_text,search_matches}`. It
  **mirrors the file viewer's find**: a bar atop the diff shows the query, match
  position/count and hints; typing is incremental, `Enter`/`↓`/`Ctrl+N` and
  `↑`/`Ctrl+P` step matches while staying in the input, `Tab` commits (the bar
  stays for highlighting), then `n`/`N` step relative to the cursor
  (`cr_search_step` scans from the selection + wraps, like `next_match`). `Esc`
  clears the search (a second `Esc` closes the review). Matched runs highlight in
  place via `ui::highlight` (on a matched diff line the hit replaces syntax colour
  for that line). State is `CodeReviewState::search: Option<ReviewSearch>` (the
  position is derived from the selection, not stored), captured before the global
  lookup like compose / the target picker. Side-by-side rows navigate but aren't
  substring-highlighted (a v1 follow-up); folded (reviewed) files contribute only
  their header until expanded.
- **Colours.** Dedicated theme keys `diff_added`/`diff_removed` (line fg) and
  `diff_added_bg`/`diff_removed_bg` (a subtle full-row tint) — added to
  `ThemePalette` (every preset derives them; bg blended toward `app_bg` via
  `blend_rgb`) and overridable per custom theme. Classification badges reuse the
  status/accent/danger palette colours, so the whole view is theme-aware.
- **Review targets** (`t` / the Target footer button). The diff can show the
  whole branch (`<base>..HEAD`, the default), the **uncommitted working changes**
  (`git diff HEAD`), or a **single commit** (`git show`) — mirroring tuicr's
  `-r`/`-w`/commit targets. An in-view picker lists Working, Branch, and each
  commit in `<base>..HEAD` (`git log`); selecting one (keyboard ↑/↓/Enter **or a
  mouse click** on the entry — `render_target_picker` returns a `RowHitbox` per
  entry, recorded as `ClickAction::ReviewTarget(i)` → `App::cr_select_target`)
  recomputes the diff (`ReviewTarget`, `build_target_diff`,
  `git::{diff_working_on,show_commit_on, list_commits_on}`). A session with no
  resolvable base defaults to the working-changes target.
- **Multi-repo sessions.** A multi-repo session reviews **all** its worktrees at
  once: the diff is built per repo and concatenated, with each file path
  namespaced `"<repo>/<path>"` so files, comments, and "reviewed" marks stay
  unambiguous; the changed-files column shows the repo-qualified paths. Each repo
  resolves its own base (the session base if that branch exists there, else that
  repo's default branch); the commit picker lists commits across every repo,
  repo-tagged. A commit target scopes to its one repo. State is
  `Vec<ReviewRepo>` on `CodeReviewState`; the diff is assembled by `build_files`.
- **Diff model + parser.** Pure data in `session::review` (`DiffFile`/`DiffHunk`/
  `DiffLine`, `Classification`, `CommentAnchor`, `ReviewComment`) with a
  unit-tested `parse_unified_diff`. `git::diff_against{,_on}` runs `git diff`
  (local or over SSH). The diff types live in `session` so `ui` can render them
  without importing `git` (architecture rule).
- **Syntax highlighting.** The unified diff body is syntax-highlighted by a
  small dependency-free lexer (`ui::syntax`: comments / strings / numbers /
  keywords / capitalised types), themed from the palette. Add/remove stays on the
  gutter `+`/`-` sign + the row tint, so the code text itself carries the syntax
  colours (GitHub-style). Side-by-side keeps plain add/remove colouring.
- **Comments.** Line / file / review-summary level, each with a classification
  (issue / suggestion / note / praise, colored badges). Composed in an **in-view
  box that floats inline at the selected line** (`render_compose_inline` anchors
  it to the line's screen row, falling back above/below as room allows) — a
  `ComposeState` sub-mode, not a separate modal. State lives in
  `app::code_review::CodeReviewState`.
- **Reviewed marks.** `r` / `R` toggle a file / hunk as reviewed (`✓`); `r`
  resolves the file from **any** row inside it (line, hunk, header, or a comment),
  not just the file header.
- **Persistence.** `review_comments` + `review_marks` tables (schema v38) keyed by
  session id (`storage::review`); the worktree's fork point is the write-once
  `sessions.base_branch` column (targeted accessors, like `hook_state`), set at
  spawn. Reviews are kept open per session across switches (like the shell view).
  Legacy/NULL base falls back to the repo's default branch.
- **Export.** No GitHub/GitLab submit (intentionally out of scope). Instead:
  `y` copies the review as markdown to the clipboard, and `e` (Send→Agent) pastes
  the compiled review into the session's agent as a prompt to address it — the
  review → agent → re-review loop, the orchestrator-native equivalent of submit.
- **Async diff build.** Opening/retargeting a review runs its git pipeline
  (base resolution, commit listing, the diffs — over SSH for a remote session)
  on a background worker with a "Building diff…" loading state, applied by
  `App::poll_review_build` per tick — the pane opens instantly (ADR-P8,
  `docs/PERFORMANCE.md`).
- **v1 follow-ups** (named, not silently dropped): range/multi-line comments,
  token-level intra-line word diffs on a paired row (v1 aligns whole lines
  positionally, not sub-line), grammar-aware syntax
  highlighting (v1's lexer is heuristic + language-agnostic), horizontal
  scroll in the **side-by-side** layout (wrap now works there; paired rows still
  pin `h_scroll = 0`), per-side search-match highlighting in
  side-by-side (v1 navigates but doesn't substring-highlight paired rows),
  auto-revealing a horizontally-scrolled-off search match, and
  search-match highlight across a wrap-boundary seam.

---

## Automations

> **CLI only, but they still fire.** There is no automations pane, and the
> interface has no in-TUI scheduler — the tmux heartbeat keeper runs due
> automations whether or not thurbox is open, which is what keeps every extension
> working. Author and inspect them with `thurbox-cli automation`. The keeper's 60 s
> cadence is the current resolution; a ~1 s in-TUI pass is owed.

In 1.x, `Ctrl+P` opened the automations list (the chord is the command
palette now). An **automation** is a named,
enable/disable-able task that fires on a schedule (one-shot or
recurring) and, when it fires, either pastes a prompt into an
existing session (**send**) or spawns a new session — optionally on
a fresh git worktree — and prompts it (**spawn**). This is the
Thurbox analogue of "scheduled agent runs": queue follow-up
prompts, run nightly maintenance, or kick off a fresh triage
session every weekday morning.

Automations replace the older one-shot "scheduled commands"
feature; a one-shot is simply an automation with a `once` schedule.

### Schedules

A schedule is either:

- **once** — fire a single time at an absolute timestamp
  (`at:<unix_millis>`), then disable itself.
- **cron** — a standard 5-field Unix cron expression (day-of-week
  `0`–`6`, `0` = Sunday). Friendly presets compile to cron:
  `hourly`, `daily`, `weekdays`, `weekly`, combined with an
  `HH:MM` time and optional IANA timezone (DST-correct via
  `chrono-tz`; defaults to system local time).

`next_run_at` (unix millis) is computed from the schedule and is
the dispatcher's scan key. After each fire it is recomputed; a
spent one-shot clears it and disables the automation.

### Actions

- **send** — bracketed-paste the prompt into the target session,
  followed by a deferred Enter. Skipped (and logged as such) if the
  target session is not currently running.
- **spawn** — create a session named `auto-<id>` (reusing it on
  later fires, including after a TUI restart where it is restored
  by name), optionally on a worktree off a base branch, with the
  chosen agent. The prompt is delivered after a short boot delay so
  the agent CLI has time to start. Worktree provisioning is
  **idempotent** (`git::create_or_attach_worktree`): if the session
  was closed but its worktree/branch still exist, a later fire
  reuses them rather than failing with "branch already exists".

### Execution model

Automations fire from **three** places, all going through the same
`thurbox-cli automation tick` logic and made safe by **claim-based
firing** (see below):

1. **TUI tick loop** (`process_automations`, ~1 s cadence) — while
   the TUI is open. On startup it runs an immediate catch-up pass
   so runs missed while the TUI was down fire once on boot.
2. **tmux heartbeat keeper** — a detached `automation-heartbeat`
   window (armed on TUI startup and on `thurbox-cli automation
   create`) that loops `thurbox-cli automation tick` every 60 s.
   Because it is a live tmux window it also keeps the tmux server
   alive, so automations — **including spawn** — fire even after
   the TUI is closed and even with no other sessions open. This
   restores (and generalizes) the old scheduled-command behavior
   of firing while the TUI is shut down.
3. **Optional OS timer** — `packaging/systemd` / `packaging/launchd`
   units run the same `tick` for reboot-proof, tmux-independent
   firing. Opt-in.

**Claim-based firing (no double-fire).** Before acting, every firer
performs an atomic compare-and-swap
(`Database::claim_due_automation`): it advances `next_run_at` *only
if* the row still holds the value it observed as due. Exactly one
firer wins; the rest skip. So the TUI, the keeper, and an OS timer
can all run at once without an automation firing twice. Ordering is
claim-then-act (at-most-once): a crash between claim and side effect
loses a run rather than duplicating one.

**Headless send vs spawn.** `send` types into the still-alive tmux
window (`send_prompt_now`). `spawn` creates the session headlessly
(`spawn_session_headless`); the prompt is delivered via a short
deferred `tmux run-shell` timer once the agent boots, and the TUI
adopts the `auto-<id>` session by name on its next startup. All of
this is local-tmux scoped today; a future remote/SSH `SessionBackend`
would plug into the same dispatch seam.

### Automations pane

A dedicated **Automations** pane sits beneath the session list in
the left column. It is **always present** (showing `none` when
empty) as long as the column is tall enough for both lists; its
height grows with the automation count (capped). Each row reads
`● name — schedule · action · next-run`. It is treated as **part of
the session pane**: it forms one continuous vertical list with the
session list, so `j` past the last session drops focus into the
pane and `k` at the top automation hands focus back to the last
session. Once focused: `j`/`k` select, `Space` toggle enabled, `r`
run-now, `d` (or `Ctrl+D`) delete, and **`Ctrl+N`/`n` create a new
automation** (works even on an empty pane).

The pane behaves **exactly like the session list**, with the
central pane as its terminal-equivalent: while the pane is focused,
the central pane shows a **single editor** for the selected
automation (a live, read-only-looking preview — no separate "info"
screen). Pressing **`Enter`** (or **`Ctrl+L`**, or `e`) moves focus
*into* that editor — just like `Enter`/`Ctrl+L` on a session focuses
its terminal — where you can change fields; **`Ctrl+H`** (or `Esc`)
returns to the list. `Enter` in the editor saves; `Esc` discards.
`Ctrl+E` toggles the automation's enabled flag from inside the
editor (the global file-viewer binding is suppressed there).

The scoped automation's **run history** is shown beneath the editor:
each row reads `<status> <clock time> <relative age> <detail>` with
the status (`ok`/`error`/`skipped`) colour-coded and bold. Press
`Ctrl+L` again (from the editor) to focus the history panel, then
`j`/`k` to move the cursor over runs; the panel footer shows its
shortcuts — **`r` runs the automation now**, **`Enter` jumps to the
session that run touched** (the send target / spawned session, when
it's still open), `Esc` returns to the editor. While in this whole
context the session list above
de-emphasises itself (no accent border, no selected-row highlight)
since the active session is irrelevant there.

`Ctrl+L`/`Ctrl+H` cycle **within the current context only** — the
automation ring is `Automations → editor → run history` and wraps
back to `Automations` (it never jumps off to a session; returning to
the list discards unsaved edits, just like `Esc`). The session ring
is the usual `SessionList → Terminal` (+ file viewer). Switching
*between* the two contexts is done with `j`/`k` in the left column,
not the focus cycle.

### List + editor (1.x)

`Ctrl+P` opened the same set over the full list (a modal, available
at any width). Keys: `n` new, `e`/`Enter` edit, `Space` toggle
enabled, `r` run-now, `d` delete, `Esc` close.

The editor avoids typing schedules by hand. **Trigger** is a
selector cycled with `←/→` — `once`, `hourly`, `daily`,
`weekdays`, `weekly`, or `cron` — and the form adapts to it:

- `once` → an **In** delay field (`30m`, `2h`, `1h30m`, `1d`).
- `hourly` → a **Minute** stepper.
- `daily`/`weekdays` → **Hour** + **Minute** steppers.
- `weekly` → a **Weekday** selector + Hour/Minute.
- `cron` → a raw expression field for power users.

**Action** is a `‹ send ›`/`‹ spawn ›` selector. For **send**, a
**Target** selector (also cycled with `←/→`) lets you pick which
running session receives the prompt — it defaults to the active
session and lists every session; saving is rejected if none exist.
For **spawn**, the **Repo**/**Worktree**/**Agent** text fields
appear instead (a leading `~` in the repo path is expanded).

`Hour`/`Minute`/`Weekday`/`Action`/`Target` are steppers/selectors
(`←/→` adjust, wrapping); `Tab`/`↑↓` move between fields; `Space`
also adjusts the focused selector/stepper; `^E` toggles enabled;
`Enter` saves. A live **next:** line previews when the automation
will fire (or shows the validation error for the current input).
Editing an existing automation reverse-maps its cron back into the
structured fields where it matches a known preset shape; otherwise
it opens as raw `cron`.

### Persistence

Automations live in the `automations` SQLite table (`name`,
`enabled`, `schedule_kind`/`schedule_spec`, `timezone`,
`action_kind` plus action columns, `prompt`, timestamps,
`last_run_at`, `next_run_at`), with a partial index on
`next_run_at` (where enabled and non-null) for the due-scan. Each
fire appends to `automation_runs` (`status` = success/skipped/error
plus a free-text `detail`) for history.

### Headless access (`thurbox-cli`)

`thurbox-cli automation` (alias `auto`) provides
`create`/`list`/`show`/`edit`/`remove`/`run`/`runs`/`tick` without
the TUI, sharing the same tables. `run` marks an automation due;
`tick` fires all currently-due automations headlessly (this is what
the tmux keeper and the optional OS timers invoke).

---

## Tasks (todo list)

> **CLI only.** There is no tasks pane. The data, the storage and the agent
> linkage are unchanged, so `thurbox-cli task` does everything below and scripts
> and extensions that used tasks still work. A pane is owed.

A **task list** of todo items that can be **connected to a coding
agent**. Tasks deliberately reuse the automation **Send/Spawn** action
model: triggering a task either pastes its title into an existing
session (`Send`) or spawns a new session — optionally on a fresh
worktree — seeded with the title (`Spawn`). A task with no action is a
plain local todo. This keeps tasks and automations on one shared
dispatch path (`App::spawn_and_prompt`).

### Why mirror automations?

The agent linkage a task needs (*"send this to an agent"* / *"spin up
an agent for this"*) is exactly what `AutomationAction` already models.
Rather than a parallel `TaskAction`, a task stores
`Option<AutomationAction>` — the `Option` adds the only new case
(unconnected local todo). One enum, one column layout, one fire path.

### Where it lives in the UI

Tasks render in a **toggleable right-side column** that sits between
the terminal and the file viewer — it behaves exactly like the file
viewer pane. **F5**/`Ctrl+W` shows and hides it (showing it also
focuses it); while visible it is a stop in the session focus ring, so
`Ctrl+L`/`Ctrl+H` cycle `SessionList → Terminal → TaskList →
FileViewer` (each extra column appears only when shown). The column is
a 20% slice added by `compute_layout` at width ≥ 120.

The panel is focusable (`InputFocus::TaskList`). Its title and border use
the shared focus styling (highlighted title + accent border when focused),
matching the session list and file viewer. Checkbox glyphs show status
(☐ todo / ◐ in-progress / ☑ done). Searching/filtering is handled by the
global `Ctrl+/` search, not a per-panel `/`.

**Editing happens in the central pane, like automations — not a modal.**
Selecting a task previews its editor in the central pane; `Enter`/`e`
focuses that editor to change fields; `Enter` saves and returns to the
panel, `Esc` discards and returns. Beneath the editor a read-only
**Details** panel shows the task's agent linkage, status, source, and
created/updated times (tasks have no run history, so this takes the place
of the automations' run-history panel). The action field cycles
Local → Send → Spawn.

Focused keys: `j`/`k` select (live-preview the editor), `n` new,
`e`/`Enter` edit in the central pane, `Space` cycle status, `r` run the
action, `d`/`Ctrl+D` delete, `Esc` leave.

### Persistence

Tasks live in the `tasks` SQLite table (added in schema **v25**; the
markdown `description` column followed in **v26**): `title`,
`description`, `status`, the automation action columns (`action_kind`
nullable for local todos), `source`/`external_id`/`external_url`,
timestamps, and a
`deleted_at` soft-delete marker, with a partial index on `status`.
Mutations are recorded in `audit_log` under `EntityType::Task`. Tasks
do **not** join the cross-instance `SharedState` (like automations) and
have **no** run-history table.

### External sync (deferred)

The `source`/`external_id`/`external_url` columns are scaffolding for a
sync with an external tracker via your own importer. Local
tasks use `source = "local"`; imported tasks will slot in with no
migration. No fetch logic ships yet.

### Headless access (`thurbox-cli`)

`thurbox-cli task` (alias `todo`) provides
`create`/`list`/`show`/`edit`/`remove`/`run`. `create` with neither
`--session` nor `--repo` is a plain local todo; `run` triggers the
task's Send/Spawn action headlessly (spawned sessions are named
`<title> · #<id>` via `Task::spawn_session_name` — the human title reads
straight in the session list while the trailing `· #<id>` tag keeps the
tmux window name unique and lets the task relink to its session — adopted
by the TUI on next startup; `Task::matches_spawn_session` recovers the
owning task from that tag and also recognizes the legacy
`task-<id>-<slug>` / bare `task-<id>` forms, so a since-edited title still
relinks).

---

## Flow Extension (experimental)

> **Status:** brand-new and under active testing — the spec, scripts,
> and installer are all expected to change between releases.

An opt-in add-on (`extensions/flow/`) that composes the task list,
sessions, worktrees, and automations into a **focus-protecting triage
workflow**: a dedicated cheap *flow session* captures brain-dumps into
tasks, dispatches the dispatchable ones to worker sessions, monitors
them, grooms the backlog, and ends every reply with the single next
thing to focus on (`🎯 Next: …`).

### Agent-agnostic by construction

Nothing in the extension names a vendor:

- The behavior is a plain context file, `FLOW.md`, installed into the
  flow home (`~/.config/thurbox/extensions/flow`) and surfaced to whichever CLI runs the session
  via symlinks to each CLI's context convention
  (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`).
- The triager and workers are **agents.toml aliases** — `flow`,
  `flow-worker` (default), `flow-worker-heavy` (long/hard work) — that
  the installer seeds with defaults and the user remaps freely.
- All orchestration goes through `thurbox-cli` (`task create/run`,
  `session capture/send`, `automation create`) plus `jq`; the core
  binary has no flow-specific code.

### Dispatch model

Dispatch is **eager**: capture creates the task *and* spawns its
worker in one atomic helper call (`create-task.sh`); workers push a
`result` message back to the flow session when they finish so a freed
capacity slot dispatches the next task immediately. Flow is purely
event-driven — there is no scheduled automation; a manual `tick`
remains the safety net that catches crashed workers and stale state.
Workers always get a
`flow/<task-slug>` worktree branch on git repos, so they never dirty
the main checkout and parallelize per repo. Completion is detected by
task status (workers self-mark done) with an orchestrate-style
`===RESULT===` JSON sentinel as the fallback, parsed from
`session capture` output.

### Install

Flow installs with the generic extension installer —
`thurbox-cli extension install flow` — which reads flow's
`extension.toml` manifest: it lays down the flow home, registers the
agents.toml aliases, creates the dedicated `flow` session, and marks
the extension active so it **self-heals** if deleted. `extension
uninstall flow [--purge]` reverses it. The
`extensions/flow/install.sh` curl one-liner is now a thin shim over the
CLI. See the generic mechanism (manifest format, lifecycle commands,
self-heal) in `docs/CONFIG.md` and `extensions/flow/README.md`.

### Sibling extensions

Two more ship in `extensions/`, both built the same agent-agnostic way
(manifest + scripts + a dedicated session/automation that self-heals):

- **`forge`** — a workflow analyst. A weekly `forge-scan` mines your
  tasks/sessions/automations for **recurring patterns** and writes
  ready-to-apply `thurbox-cli automation` proposals; it *proposes, never
  imposes* (nothing is created until you `apply <slug>`, and apply refuses
  any non-`thurbox-cli` command). `thurbox-cli extension install forge`.
- **`ci-shepherd`** — watches your open change requests (GitHub PRs /
  GitLab MRs / Bitbucket PRs and **any other git forge**, decided by the
  agent at runtime) and dispatches a `shepherd-worker` fixer for each with
  **failing CI** or a **changes-requested review**.
  `thurbox-cli extension install ci-shepherd`.
- **`renovate`** — keeps local repos on up-to-date dependencies. A weekly
  `renovate-tick` dispatches a `renovate-worker` per watched repo that runs
  **Renovate's local platform only** (`--platform=local`, no bot/token/PR),
  tests the bumps, commits to a fresh `renovate/updates-<ts>` branch, and
  opens a review PR. Per-repo `strategy` (patch/minor/major/all) layers onto a
  global `renovate-config.json`. `thurbox-cli extension install renovate`.
- **`ui-skill`** *(built-in, on by default)* — the only one that ships no
  session, no automation and no agent. It installs a single **agent skill**,
  `thurbox-ui`, into each coding CLI's personal skill directory
  (`~/.claude/skills/`, `~/.codex/skills/`, `~/.config/opencode/skills/`,
  `~/.copilot/skills/`, `~/.agents/skills/`, each guarded so a CLI you do not
  have is skipped), so an agent in **any** session knows how to change thurbox's
  own interface — where it lives, how to check an edit, and what the sandbox
  withholds. It replaces attaching the interface directory to every session as
  an extra repo: a skill loads only when the request is about the TUI. Like
  `hooks` it is embedded in the binary and auto-activated, because someone who
  does not already know the interface is editable will not go looking for the
  extension that says so. `thurbox-cli extension deactivate ui-skill` turns it
  off.
- **Tracker import** — no longer an extension. Four per-provider trees
  (`github-issues`, `gitlab-issues`, `linear`, `jira`) were removed: they were
  near-identical, each carrying one provider's API shape, for a job that is a
  `curl` and an upsert. The support that made them work is generic and stays —
  `task --source/--external-id/--external-url`, `get_task_by_external_id`, the
  `idx_tasks_external` index, and the `Exec` automation action — so a scheduled
  `Exec` running your own script does the same thing. Dedup is on
  `(source, external_id)` and only open-vs-done is authoritative on the way in, so
  a local `in_progress` is never clobbered. No provider name is in the binary, by
  design (ADR-20).

---

## Global Search

> **Rebuilt as a plugin.** The strip is `ui/plugins/65_search.lua` and no longer
> floats: it is a full-width slot the arrangement carves above the chrome bands,
> because it highlights matches *inside* the panes it is searching and a modal
> would cover the thing it is pointing at. Sessions is the only scope with a pane
> today; the tasks, automations and files scopes went with their panes. The
> rationale below is kept because it is what a scope being added back needs.

`Ctrl+/` (the near-universal "search" chord) opens a **non-modal strip** that
searches every scope with a pane. The opener is rebindable like every other
chord, through the registry the F1 help renders.

### Scopes

- **Sessions** — name, agent, branch and repo (fuzzy), plus the live terminal
  **screen text** so you can find *which session* mentioned a string ("deploy
  failed", an error, a file path) and switch straight to it.

A result carries the pane it belongs to, so a returning surface is a scope added
and nothing else changed.

Matching is subsequence via `ui/lib/fuzzy.lua`, shared with the session list so
the two cannot disagree. Screen text is matched as a **substring** rather than a
subsequence — fuzzy over a whole screen matches nearly everything — and is
skipped for a session whose metadata already matched.

Terminal text is a **want**, not a standing cost: the pane leaves its query in
`store` under `want_content` and the kernel serves `thurbox.content` only while it
is asking (`kernel::terminal::WANT_CONTENT`, capped at `CONTENT_LINE_CAP` = 500
lines, the bound v1 used). No interface pays for every agent's screen on every
frame.

### Live preview & cancel

Moving through results (`↑`/`↓`) **previews** the selection in place: the owning
pane's cursor follows the highlighted result, so you see where `Enter` would land
without leaving the search box. `Esc` puts back what you were looking at; `Enter`
commits the jump and focuses the result's pane.

### Live in-place highlighting

As you type, matches highlight **where they live**: the session list highlights
the matched characters on matching rows and **dims** the rows that don't match —
the same treatment the list's own filter uses. Nothing is reprinted in the strip,
which is the point of a strip rather than a float.

### One deliberate divergence from v1

v1 also took `Ctrl+P`/`Ctrl+N` inside the strip, because its search focus captured
input ahead of the keybinding table. Here every chord goes through one registry
where a plugin-scoped claim does not outrank a global one, so declaring them would
take `Ctrl+N` from new-session everywhere. Recorded in `tests/keymap.rs`.

---

## Feature Flags (`[features]` in settings.toml)

> `code_review`, `file_viewer`, `info_panel` and `tasks` gate surfaces the interface
> no longer draws (`tasks` still gates its CLI). They are accepted and preserved so
> an existing file does not fail `thurbox-cli config validate`, and are not listed
> in the settings panel, since a row that gates nothing reads as broken.

Whole features can be switched off declaratively: `tasks`,
`automations`, `file_viewer`, `global_search`, `info_panel`,
`shell_pane`, `mouse`, `notifications`, `soft_delete` — all default
`true`. `soft_delete` is the odd one out: it is not a pane gate but a
behaviour switch for the TUI `Ctrl+D` delete (soft-delete with a
`Ctrl+Z` undo window when on; a confirmation-gated hard delete when
off — see *Explicit close vs quit*). Two flags reach the network and
were opt-in before 1.0 — now both default on:
`version_check` (the "update available" badge +
`thurbox-cli version --check`) and `auto_update` (silent self-update on
startup + `thurbox-cli update`). See `docs/CONFIG.md`.

**Decision: flags are UI-level gates, not data switches.** A disabled
feature hides its pane, consumes its keybinding with an explanatory
status toast (the chord never reaches the PTY), and contributes no
global-search results — but its data and the `thurbox-cli` surface
stay fully functional, so flipping a flag back on is lossless. The one
deliberate exception is `automations = false`, which also stops the
TUI firing due schedules and arming the tmux heartbeat at startup —
"disable automations" should actually stop scheduled work, not just
hide a list. Explicit CLI automation commands (and an already-armed
keeper window) keep working, because typing a command is unambiguous
intent. `mouse = false` is similarly a hard gate at the boundary:
terminal mouse capture is never enabled (so the terminal keeps its
native selection/URL handling) and any stray mouse event is dropped
before dispatch.

The F1 help panel intentionally keeps disabled actions listed: hiding
rows would break the selection-index contract with
`Action::rebindable_in_order()`, and the toast already explains why a
chord did nothing.

---

## Settings Panel (`Ctrl+,` / `F6`)

> Now `kernel::modals::settings` — kernel-owned chrome that plugins contribute rows
> to. Core settings still write `settings.toml` through `toml_edit`, and whether a
> row applies live is *asked of* `Settings::restart_only_differs` rather than
> recorded beside the field. The rows show `Config::on_disk` — the file — because a
> restart-only change lives only there until the next launch; drafting from what is
> in force made every later save revert it.

`Ctrl+,` (rebindable `Action::OpenSettings`; `F6` alternate) opens a
centered Settings modal that views and edits **all of settings.toml** —
the `[features]` toggles, the `[notifications]` knobs, and the scalars —
without hand-editing the file.

**Why apply-on-save, not live preview.** The modal edits a working-copy
`draft` and writes it back only on `Ctrl+S` (`Esc` discards). Persistence
stays in `settings.toml`, written through a `toml_edit::DocumentMut` so
the seed's documentation comments survive the round-trip.

**Why some rows take effect immediately and others need a restart.** The
feature flags that gate UI panels are read every frame, so a save copies
them into the live `App.features` and they apply at once. Everything else
is read once at startup from a write-once `OnceLock` that can't be
re-applied in-process; those rows are marked `⟳`, and a save that touches
one toasts "some changes apply after restart". The canonical comparison
(`Settings::restart_only_differs`) is shared by the toast and the reload
path so the two never disagree.

**Why live-reload the file too.** `settings.toml` is watched by mtime
(like `agents.toml` / `keybindings.json`): an external edit — a
hand-edit, or the panel in another instance — re-applies the live feature
flags and toasts (noting a restart when only restart-only fields
differ). The panel's own write marks the file saved so the poll doesn't
re-toast it.

---

## Update Notifications & Auto-Update

Two opt-in `[features]` flags (default `false`, because they reach the
network — see *Feature Flags*) cover staying current:

- **`version_check`** adds an "update available" badge in the TUI header
  and the `thurbox-cli version --check` query. The latest release is
  fetched from GitHub and cached for 24 h, so it costs at most one
  request a day.
- **`auto_update`** adds a silent self-update on TUI startup and the
  `thurbox-cli update` command, which downloads, checksum-verifies, and
  replaces the installed binaries with the latest release. `--force`
  bypasses the up-to-date and dev-build guards.

Both are on by default for 1.0 so a fresh install stays current on its
own; set them to `false` if you'd rather make no network calls or have
thurbox never mutate its own binary unless you ask.

---

## Error Handling UX

### Rule: never crash, never modal

Errors are shown in the status bar footer as transient messages.
They do not block interaction, do not require dismissal, and
auto-clear after a timeout or on the next successful action.

**Why non-modal?**

- Modal error dialogs in a TUI are jarring — they steal focus from
  the terminal where the user is working.
- Most errors are recoverable (session failed to start, PTY read
  error). Showing them passively lets the user decide when to act.
- Fatal errors (can't initialize terminal) are the only case where
  the app exits, and those happen before the TUI is even rendered.

---

## Responsive Layout

> Breakpoints are no longer compiled in: `ui/layout.lua` decides the arrangement and
> may branch on width however it likes. The tiers below are what the shipped
> `layout.lua` still does, so they remain what a user sees.

The info panel (`Ctrl+B`) and file viewer (`Ctrl+E`) are the
optional columns that appear at wider widths:

### Breakpoint Rationale

| Width | Layout | Why |
|-------|--------|-----|
| `<80` | Terminal only | Sidebar would leave <60 cols — too narrow |
| `>=80` | Sidebar + terminal | 20-col sidebar + 60-col terminal min |
| `>=120` | Sidebar + terminal + info | Terminal still gets ~70+ cols |

### Why not user-configurable?

Configurable breakpoints add UI, storage, and edge-case complexity
for minimal gain. The fixed values cover standard terminal sizes
(80, 120, 160+). If a user resizes their terminal, the layout
adapts instantly. Custom breakpoints can be added later if real
demand emerges.

---

## Git Worktree Integration

Sessions can optionally run inside git worktrees for branch
isolation. This is opt-in by marking a repo with `w` in the repo
picker.

### Flow

1. `Ctrl+N` triggers session creation and opens the repo picker.
2. Marking a repo with `w` in the picker routes through the
   worktree branch flow.
3. A base branch selector lists local branches from the selected
   repo.
4. Selecting a base branch opens a prompt for the new branch name.
5. Confirming creates a new git branch (from the selected base) in
   a worktree and spawns the session inside it.
6. Mixed sessions are supported: worktree-marked repos get a new
   branch while normal repos are added as-is.

### Worktree storage

Worktrees are created at
`<repo>/.git/thurbox-worktrees/<sanitized-branch>`, where `/` in
branch names is replaced by `-`.

### Cleanup behavior

- Closing a worktree session (`Ctrl+D`) automatically removes the
  worktree via `git worktree remove --force`.
- Quitting Thurbox (`Ctrl+Q`) preserves worktrees on disk so they
  can be resumed on next launch (see [Session Persistence](#session-persistence)).
- Cleanup errors are logged but do not block session close or app
  shutdown.

### UI indicators

- **Terminal title**: Worktree sessions show the branch in the
  title bar: `my-session [feature/foo] [Running]`.
- **Session list**: Branch name appears next to worktree sessions
  with a green `[branch]` badge.
- **Info panel**: Shows a "Worktree" section with branch name and
  worktree path when viewing a worktree session.

---

## Worktree Sync

`Ctrl+S` synchronizes all worktree sessions with their upstream
default branch. The operation runs in the background — the TUI
stays responsive throughout.

For a **remote** session (SSH/WSL) the worktree lives on the host,
not on the local machine, so every git subcommand runs *on the host*
via the same transport-neutral launcher the rest of git uses
(`git::sync_worktree_on(host, …)` → `git_command(host, …)` → `ssh …`
/ `wsl.exe …`). Syncing locally would fail with "no such file or
directory" because the remote worktree path doesn't exist here. Local
sessions pass `host = None` and are unchanged.

### Algorithm

Sessions are grouped by repository path so that worktrees sharing
the same `.git` directory are synced sequentially (avoiding git
lock contention). Different repositories sync in parallel.

Per-worktree steps:

1. **Clean stale index locks** — removes `.git/index.lock` from
   crashed git processes (see below). **Local worktrees only** — the
   sweep stats the local filesystem (`/proc`, mtime), so it is skipped
   for a remote host.
2. **Stash** — saves uncommitted changes so rebase can proceed on
   a clean tree.
3. **Fetch** — `git fetch` from origin.
4. **Rebase** — `git rebase origin/main` onto the latest upstream.
5. **Stash pop** — restores the stashed changes. If rebase fails
   (conflict), the stash is popped before reporting the conflict.

**Why stash instead of requiring a clean tree?** Agent sessions
frequently have uncommitted work in progress. Requiring a clean
tree would make sync unusable in the most common case.

**Why group by repo?** Worktrees linked to the same repository
share a single `.git` directory. Running concurrent git operations
against the same `.git` causes index lock conflicts. Sequential
processing within a repo group eliminates this.

### Stale index lock cleanup

Before stashing, Thurbox checks for stale `.git/index.lock` files
left behind by crashed git processes:

- **Linux**: reads the PID from the lock file and checks
  `/proc/{pid}` — removes the lock if the process is dead.
- **Fallback** (all platforms): removes locks older than 60 seconds
  based on file mtime.

If the first stash attempt fails with a lock-related error,
Thurbox retries up to 3 times with increasing delays (100 ms,
500 ms, 1 s) after cleaning stale locks.

### Results

Each worktree reports one of three outcomes:

- **Synced** — rebase succeeded, stash restored.
- **Conflict** — rebase failed due to merge conflicts. The conflict
  details are sent to the session's agent as a prompt asking it to
  resolve the rebase.
- **Error** — fetch or stash failed. The error message is shown in
  the status bar.

The status bar summarizes results: `"3 worktree(s) synced"` or
`"2 synced, 1 conflict(s)"`.

### Non-blocking execution

Sync runs on background threads via an `mpsc` channel. The main
event loop polls `try_recv()` each tick to collect results as they
complete. The TUI remains fully interactive during sync.

---

## Session Persistence

Sessions run inside a dedicated tmux server (`tmux -L thurbox`)
and survive thurbox crashes, restarts, and even multiple concurrent
thurbox instances.

### How it works

- Sessions spawn as tmux windows in the `thurbox` session. The
  tmux pane keeps running regardless of thurbox's lifecycle.
- On every session spawn, Thurbox assigns an `agent_session_id`
  (UUID v4) via the agent CLI's `--session-id` flag. This tells
  the agent to use a stable conversation ID from the start.
- On shutdown (`Ctrl+Q`), session metadata (including backend IDs)
  is written to the SQLite database at
  `$XDG_DATA_HOME/thurbox/thurbox.db`. Thurbox detaches from each
  session without killing it.
- On next startup, Thurbox discovers existing sessions from tmux,
  matches them to persisted metadata by `backend_id`, and adopts
  them — reconnecting to the live tmux panes with terminal content
  intact. Unmatched persisted sessions fall back to
  `--resume <session-id>` to create new tmux panes.
- A persisted `backend_id` is a **hint, not a fact**. tmux hands out
  fresh pane ids every time its server starts, so after a reboot every
  stored id names a pane that no longer exists — and `%1` after one
  belongs to whichever window came up first. So a pane id a window
  listing contradicts (it is not in the window this session's name
  produces) is dropped in favour of matching by name, and a session
  with neither has its agent relaunched. Trusted verbatim, it instead
  failed to adopt on `resize-window` (`can't find pane`) once per retry
  interval for the life of the process, while the relaunch that would
  have fixed it was skipped precisely *because* the row named a pane.
- External recovery is always possible via `tmux -L thurbox attach`.

### State storage

All session state is stored in the SQLite database (`thurbox.db`).
Tables include `sessions`, `worktrees`, `scheduled_commands`, and
`metadata`. The database uses WAL mode
for concurrent multi-instance access. Agent definitions are the
exception — they live in `~/.config/thurbox/agents.toml`.

### Worktree preservation

Worktrees are **not** removed on `Ctrl+Q` shutdown — they persist
on disk so the resumed session can continue working in the same
branch checkout. Worktree metadata (repo path, worktree path,
branch name) is saved in the database and reconstructed on restore.

### Explicit close vs quit

- **`Ctrl+Q` (Quit)**: Detaches from all sessions (tmux panes keep
  running), saves metadata. Sessions resume on next launch with
  terminal content preserved.
- **`Ctrl+D` (Delete)**: Soft-deletes the session — its tmux pane
  is killed and its worktree (if any) is removed. The database
  row is retained with `deleted_at` set so the deletion can be
  undone with `Ctrl+Z` (most recent) or restored from the
  `Ctrl+U` list. This is governed by `[features] soft_delete`
  (default `true`): set it `false` and `Ctrl+D` becomes a **hard
  delete** — the full teardown with no `Ctrl+Z` undo, so it is gated
  behind a confirmation modal (`Modal::ConfirmDeleteSession`) instead.
  The flag never affects `thurbox-cli session delete`, which stays soft
  unless `--force`. Either way the row **leaves the list on the
  keystroke** rather than sitting there tagged while the teardown runs:
  the session list drops any session whose `delete` is in flight
  (`live_sessions()` in `ui/plugins/10_sessions.lua`), so the cursor lands on
  the next session and a repo group whose last session went takes its
  header with it. A delete that *failed* keeps its row — the failure is
  the only thing that says the session is still there.

### Multi-instance support

Multiple thurbox instances can view the same tmux sessions. Each
instance independently connects to tmux in control mode (`-C`).
Tmux broadcasts `%output` notifications to all connected clients —
there is no primary/secondary distinction.

---

## Parent Sessions (Lead/Worker)

Sessions carry an optional `parent_session_id` (nullable column on
`sessions`, schema v30) so orchestration scripts can model a lead
session that spawns workers: `thurbox-cli session create --parent
<uuid>` sets it, `session list`/`get` expose it, and `session list
--parent <uuid>` lists direct children. In the TUI, `Ctrl+F` fork
records the source session as the fork's parent.

### Why informational-only (no cascade)

The link is metadata, not a lifecycle contract. Deleting a parent
does **not** delete or orphan-block its children — workers routinely
outlive the lead that spawned them (the lead finishes orchestrating
while workers keep coding). A dangling parent id is harmless: the
child simply renders as a top-level session again. The parent is
validated once, at creation (it must be an existing active session),
and never re-validated.

### Why nesting stays inside repo groups

The session list's primary grouping is the repo set
(`compute_session_order`), and that stays authoritative: children
nest under their parent **within** a repo group (muted `└` prefix,
depth tracked in `SessionOrder::depths`), because a lead and its
workers usually share a repo. A child whose parent renders in a
different group keeps its natural position and gets a `↳` mark
instead — reordering across repo groups would break the "one header
per repo" invariant and make rows jump between groups. Group
bubbling is unchanged: an `Attention` child still pulls its whole
repo group to the top. Navigation (`Ctrl+J`/`Ctrl+K`) shares the
same ordering function, so it walks the tree exactly as rendered.
Parent cycles can't be produced by current writers (the parent must
exist before the child, and the link is immutable), but the ordering
is still defensive: cycle members render flat rather than vanish.

---

## Inter-Session Messages (Mailbox Queue)

A general, agent-neutral message queue (`session_messages` table, schema
v32; `thurbox-cli message`) lets one session hand another a **structured
payload** — addressed to a session, with a free-form `kind` tag, a `body`,
and optional `from_session_id`/`from_task_id` provenance. It is the channel
extensions use for agent↔agent coordination; flow's clarify→plan→build
relay is the first consumer.

### Identity-aware, no ids to pass

At spawn thurbox injects each session's own identity into its environment
(`THURBOX_SESSION` = the stable `SessionId`, and `THURBOX_TASK` for
task-spawned sessions), so a `thurbox-cli` call running *inside* a session
knows who it is. `message send`/`inbox` therefore default the
sender + task provenance (and `--for`) to the caller's own identity — an
agent sends and reads its own mail with **no ids**. Replies never need a
peer's id either: `message reply <message_id> --body …` looks the original
message up and routes back to *its* sender, carrying the original task tag.
This is how flow relays a user's answer back to a worker without ever
mapping a task to a session id.

### Why push, not pane-scraping

Agent CLIs are TUIs: their output is rendered with box chrome, prefixes,
and line-wrapping, so grepping a captured pane for a sentinel is fragile
and only as timely as the next poll. The queue inverts the channel — a
worker **pushes** a clean payload (`message send`) and a `--wake` nudge
types a short `inbox` token into the recipient's pane so it drains
immediately. The payload always travels through the durable DB, never the
pane; the wake is just an idempotent "go look" (a missed or colliding wake
only delays a drain to the next nudge/tick).

### Why exactly-once and bounded

`claim_messages` is a single `UPDATE … WHERE read_at IS NULL … RETURNING`
statement: SQLite serializes writers, so the TUI, a cron tick, and a wake
nudge can drain the same inbox concurrently without ever handing one
message to two claimers or dropping one. Growth is bounded on both ends —
`enqueue_message` rejects past a per-recipient unread cap (backpressure,
not silent loss) and caps `kind`/`body` size, while a time-based retention
sweep (`prune_old_messages`, read messages older than the default window)
runs at DB open and on each `automation tick`, mirroring audit-log
pruning. The table is intentionally **not** audited — it is high-churn and
ephemeral. The same `PRAGMA data_version` polling that backs every other
table lets a future TUI inbox surface unread counts with no schema change.

### Implementation reference: storage, delivery, and CLI

The data/storage shape, delivery + backpressure guarantees, and the full CLI
surface. `CLAUDE.md` keeps the identity contract and points here.

- **Data**: `session::SessionMessage` (pure data, `session/message.rs`;
  `validate_kind_body` bounds `kind`≤32 B / `body`≤64 KiB). **Storage**:
  `session_messages` table (schema **v32**, plain-TEXT uuids, no FK — mirrors
  `tasks.target_session`), with a partial unread index + a `created_at` index.
  CRUD in `storage/messages.rs`.
- **Exactly-once delivery**: `Database::claim_messages` is a single
  `UPDATE … WHERE read_at IS NULL … RETURNING` — SQLite serializes writers, so
  the TUI, a cron tick, and a worker's wake nudge drain concurrently without
  double-processing or dropping. `list_messages` peeks without consuming.
- **Bounded growth**: `enqueue_message` enforces a per-recipient unread cap
  (`MAX_UNREAD_PER_RECIPIENT`, backpressure not silent loss) + the body/kind
  limits; `prune_messages`/`prune_old_messages` (read messages older than
  `DEFAULT_RETENTION_DAYS`) run at DB open and on every `automation tick`,
  mirroring audit-log pruning. The mailbox is **not** audited (high-churn).
- **CLI** (`thurbox-cli message`, alias `msg`) — identity-aware:
  - `send --to <uuid|name> --kind <k> [--task <id>] [--from <uuid|name>] --body
    <text> [--no-wake]` enqueues and, unless `--no-wake`, types a short `inbox`
    token into the recipient's pane (`agent::tmux::send_prompt_now`) to nudge a
    drain. **Provenance + task tag default to the caller's injected identity**
    (`THURBOX_SESSION`/`THURBOX_TASK`) so an agent passes **no ids**; `--from`/
    `--task` override.
  - `reply <message_id> --body <text> [--kind k] [--from …] [--no-wake]` —
    enqueues back to the *original message's sender* (looked up via
    `get_message`) and wakes them, carrying the original `from_task_id`. The
    replier handles only the opaque message id — never a peer's session id. This
    is how flow relays the user's answer without name-scraping.
  - `inbox [--for <uuid|name>] [--claim] [--all] [--limit N]` reads it (`--claim`
    = atomic drain); **`--for` defaults to the calling session** so an agent
    reads its own mail with no id.
  - `prune [--older-than-days N] [--read-only]`.
  - `cli::messages` resolves a session by UUID **or** name (`resolve_uuid_or_name`
    → `Database::get_session_by_name`); a `send`/`reply` with a wake also arms the
    automation heartbeat (`cli::automations::arm_heartbeat`) so a missed wake is
    still drained headless. `PRAGMA data_version` already surfaces writes to the
    TUI — no sync/`SharedState` change.

---

## Terminal Scrollback

### Scrollback buffer

The terminal uses vt100's built-in 1000-line scrollback buffer.
`Screen::scrollback()` returns the current offset (0 = at bottom),
and `Screen::set_scrollback(n)` moves the viewport. When the offset
is non-zero and new output arrives, vt100 auto-increments the
offset to keep the view pinned at the same history position. When
the offset is 0, new output naturally stays at the bottom.

### Scroll keybindings

`Shift+Up/Down` scrolls one line, `Shift+PageUp/PageDown` (or
`Alt+PageUp/PageDown`) scrolls half a page, and the mouse wheel
scrolls three lines per tick. The `Alt+Page` pair exists because
Terminal.app and iTerm2 claim `Shift+Page` for their own scrollback,
so on macOS those chords never reach Thurbox (`Fn+Option+Up/Down`
on a Mac laptop).
Any other keypress while scrolled up snaps back to the bottom
before forwarding to the PTY. This matches the mental model of
"I'm reading history, and when I start typing I'm back in the
present."

**Why Shift, not Ctrl?**

Ctrl-prefixed keys are reserved for Thurbox global commands.
Shift+arrow and Shift+Page are the conventional scrollback
keybindings in most terminal emulators (GNOME Terminal, Kitty,
Alacritty) and do not conflict with the agent CLI or shell readline.

### Scrollbar widget

A ratatui `Scrollbar` overlays the right edge of the terminal
panel (inside the border). It only appears when there is scrollback
content. The thumb position is inverted from the offset (offset 0
= thumb at bottom, max offset = thumb at top) to match visual
expectations. When scrolled up, the block title shows a `[N↑]`
indicator and the PTY cursor is hidden to avoid visual noise in
historical output.

---

## Theme System

![Theme switcher](../media/thurbox-theme.gif)

All UI colors are centralized via a semantic palette:
`session::theme_config` holds the presets and the user's overrides,
`kernel::theme::Themes` resolves one and publishes **roles** to Lua
(`ui/lib/theme.lua`). A plugin asks for `theme.accent` or `theme.muted`
rather than a colour, which is what lets one plugin look right under all
thirty-six palettes and the whole interface be re-skinned by swapping the
active one.

Thurbox ships thirty-six built-in presets — twenty-eight dark
(Default, Catppuccin Mocha, Tokyo Night, Gruvbox Dark, Doom, Nord,
Dracula, One Dark, Rosé Pine Moon, Everforest, Kanagawa, Solarized
Dark, Monokai, Ayu Dark, Ayu Mirage, Material, Rosé Pine, Oxocarbon,
GitHub Dark, Nightfox, Sonokai, Melange, Zenburn, Iceberg, Vesper,
Synthwave, Nightfly, Tomorrow Night) and eight light (Catppuccin
Latte, Tokyo Night Day, Gruvbox Light, Solarized Light, Ayu Light,
One Light, Rosé Pine Dawn, GitHub Light). Press `Ctrl+Y` (or `F4`,
which avoids terminals that
intercept `Ctrl+Y` as DSUSP) to pick one. The choice is persisted
in SQLite under `metadata.active_theme` and survives restarts;
other Thurbox processes pick it up within one tick via
`PRAGMA data_version` polling.

### The picker at this list length

Thirty-six presets (plus any custom themes) is far more than fits on
one screen, so the picker (`ui::theme_picker_modal`) is built around
the long list rather than scrolling a flat one:

- **Filter behind `/`.** The picker keeps the shared selector keys —
  `j`/`k` (plus `↑`/`↓`, `PageUp`/`PageDown`, `g`/`G`, `Home`/`End`)
  select, and `Ctrl+N`/`Ctrl+P` are accepted as alternates. Only `/`
  opens a filter sub-mode, in which letters append to a query matched
  against each theme's display name *and* its stable id (so both `rose`
  and `rose-pine-dawn` find the same entry). This mirrors the file
  viewer's and code review's find rather than swallowing every letter,
  so no key means something different here than in the other pickers.
  `PageUp`/`PageDown` step by the list's *rendered* height, fed back
  from the view each frame (`App::theme_picker_page`).
- **Two `Esc` levels.** While filtering, `Esc` closes just the filter
  and restores the full list — keeping the cursor on the theme it was
  on, so leaving the sub-mode never jumps the preview elsewhere. A
  second `Esc` cancels the picker. The header line shows the live query
  (with a block cursor) or, in navigation mode, a `/ filter themes`
  hint; either way it carries a `matched/total themes` count, so a
  query that narrows to nothing is legible instead of an unexplained
  empty list.
- **`Dark` / `Light` section headers.** Emitted at the first entry of
  each run, so filtering away every light theme also drops the `Light`
  header. Headers are rendering decoration drawn *within* their entry's
  row, which keeps selection indices, click hitboxes, and the scrollbar
  all in plain entry space — a header is never separately selectable.
- **Filtered-space selection.** `ThemePickerModal::index` indexes the
  *match* list, not the full entry list, and every consumer resolves it
  through `matches`. Refining a query keeps the cursor on the same
  *theme* when it survives the filter, so narrowing can never silently
  apply a different palette than the one previewed.
- The modal grows with its content up to ~85% of the frame, then
  scroll-windows with a scrollbar. The live swatch also previews text,
  diff, border and modal-background colours, not just the accent.

### Why centralized?

- ~50 color references were scattered across 13+ widget files.
  Changing the accent color required editing every file.
- Semantic names (accent, status, border) make the intent clear at
  each call site.
- A single palette enables user-selectable themes without touching
  widget code.

### Color categories

| Category | Purpose |
|----------|---------|
| Accent | Focused borders, selected items, highlights |
| Status | Session status indicators (busy/waiting/idle/error) |
| Text | Three-level text hierarchy (primary/secondary/muted) |
| Borders | Panel border states (focused/unfocused) |
| Domain | Semantic colors for agent name, branch name |
| Hints | Keybinding and interactive hints |

---

## Focus Levels

> **Reshaped.** The kernel owns one focus ring over whatever panes are loaded, and
> the trap it has to keep apart is `is_drawn` vs `can_focus`: a `switch` slot draws
> one occupant, so focusing an alternate is *what brings it forward*, and gating
> focus on "is it drawn?" makes an alternate unreachable. See `docs/V2-KERNEL.md`.

Panels use a tri-state focus system (`Focused`, `Active`,
`Inactive`) for clear navigation feedback.

| Level | Border | Title | Meaning |
|-------|--------|-------|---------|
| `Focused` | Thick cyan | Bold cyan | Receiving input |
| `Active` | Plain cyan | Cyan text | Contextually relevant |
| `Inactive` | Plain gray | Gray text | Background |

---

## Status Messages

Status messages have a severity level and auto-dismiss after 5
seconds.

| Level | Badge | Text color | Use case |
|-------|-------|------------|----------|
| `Error` | Red `ERROR` | Red | Validation failures, operation errors |
| `Warning` | Yellow `WARN` | Yellow | Non-blocking issues |
| `Info` | Cyan `INFO` | Gray | Success feedback ("Session saved") |

Positive feedback is shown for: session start/restart/delete/
restore, worktree sync, and theme changes.

---

## OS Notifications

Status messages are in-app and transient; OS notifications are the
out-of-app analog for the one event a user must not miss — a session
that **needs them**. When a session transitions to
`SessionStatus::Blocked` (the agent's hook reported it needs input or
approval), thurbox fires an OS desktop notification. An opt-in
`also_on_waiting` extends the trigger to the `Working → Done` (finished)
edge for when you want a nudge each time a turn completes.

### Why the transition is observed in one place

The edge is detected once per tick in `refresh_session_statuses` — the
**same** place `SessionStatus` is computed — so the notification rule
can never drift from the status dot shown in the list. It is
deduplicated per session by `min_interval_secs`, and the session you
are currently viewing is skipped by default (`suppress_for_active`),
since you don't need an alert for the pane you're already watching.

### Delivery backend (auto-detected)

The concrete backend is resolved by `detect_backend` from the configured
`[notifications] backend` (default `auto`) plus host probing. `auto`
picks **dbus** on a normal Linux desktop (a session-bus
`org.freedesktop.Notifications` socket answers), the native **macOS**
banner, and — the case the doc previously omitted — a **Windows toast**
under WSL when no dbus daemon answers (`/proc/version` carries the
Microsoft marker and `powershell.exe` is on PATH; we shell out a WinRT
toast script). The WSL path fixed a silent-failure bug: the dbus path
used to error on connect there but only log a `warn!`, so the user saw
nothing. Delivery errors now land in a process-wide slot surfaced by
`thurbox-cli notify`.

### Click-to-focus (Linux), passive banner (macOS / WSL)

On Linux the dbus action callback writes a session id to the SQLite
`metadata` row; the TUI's external-state poll reads and **deletes it
atomically** (a single `DELETE … RETURNING`) on its next tick and
switches to that session. macOS and the WSL Windows toast show the banner
but ignore clicks — modern `UNUserNotificationCenter` actions require a
signed app bundle (which thurbox is not), and a Windows toast can't call
back into WSL. **Terminal window-raising is
deliberately not implemented**: thurbox runs inside an arbitrary
terminal emulator it doesn't own, and per-emulator window control is
fragile (especially on Wayland), so the session is merely pre-selected
and the user alt-tabs back themselves.

### TUI-only lifecycle and gating

The PTY parser that observes the bell only runs while the TUI is
alive, so notifications never fire from a headless `automation tick`.
The dispatcher thread (`crate::notifications::start`) starts only when
`[features] notifications = true`, so the feature is zero-overhead when
disabled. Knobs live in the `[notifications]` block of `settings.toml`
(`also_on_waiting` / `suppress_for_active` / `sound` /
`min_interval_secs` / `backend`) — see [CONFIG.md](CONFIG.md). `backend`
forces the delivery path (`auto` / `dbus` / `windows` / `macos`) or
silently drops everything (`off`, a soft switch distinct from the
`[features]` flag, which stops the dispatcher thread entirely).

---

## Empty Terminal State

When the active session has no terminal content yet, the terminal
panel shows a centered hint box:

```text
┌───────────────────────────────┐
│ No active sessions            │
│                               │
│   Ctrl+N  New session         │
│   F1      Help                │
└───────────────────────────────┘
```

The session list is empty until the first session is created; the
active terminal can also briefly be empty during spawn.

---

## Info Panel Separators

> **Not in the binary.** The info panel went with `src/ui` and was rebuilt as
> [`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel), which
> reproduces the grouping this section argues for. Kept because that is the reason
> it argues for it.

Section boundaries in the info panel use styled `──────` separator
lines instead of blank lines, improving visual structure.

---

## Text Selection and Copy-Paste

Mouse drag selects text in the terminal panel. The selection is
confined to the active pane bounds.

- **Mouse drag**: Select text (anchor at press, cursor follows
  drag). Dragging past the top/bottom edge scrolls the grid, so a
  selection can extend beyond one screenful. A press that never
  moves is a click, not a selection — so clicking into a shell to
  focus it leaves `Ctrl+C` as the shell's interrupt — and any key
  press or wheel tick drops a selection (the key still does its job).
- **`Shift`+drag**: Bypasses thurbox entirely and uses your
  **terminal's own** selection. Most emulators reserve Shift for this
  while an application holds the mouse; use it when you want the
  terminal's native copy behaviour (including its own clipboard
  integration) instead of thurbox's.
- **`Ctrl+C`** (with active selection): Copies the selection. See
  the transport section below.
- **`Ctrl+C`** (no selection): Forwarded to the terminal as SIGINT.
- **`Ctrl+V`**: Pastes from the local clipboard. When a modal text
  input (worktree/session name, repo-picker path or search,
  automation editor) or an in-pane editor (task/automation) is
  focused, the text is inserted into that field instead of the PTY
  (`try_paste_into_modal_input`; single-line inputs strip embedded
  newlines, the multi-line task description keeps them). While **any**
  modal is open the paste is swallowed so it can never leak into the
  terminal in the pane behind the overlay; otherwise it pastes into
  the active PTY.
- **`Ctrl+Shift+V`** (your terminal's paste): the way to paste when
  thurbox runs over SSH — see "Pasting over SSH" below.
- **`Cmd+C` / `Cmd+V`** (macOS): the same two actions, declared beside the
  Ctrl pair because `Ctrl+C` in a terminal means interrupt. They arrive only
  under the kitty keyboard protocol (iTerm2 3.5+, kitty, WezTerm, Ghostty),
  which thurbox pushes at startup; Terminal.app delivers no Cmd chord. The
  **emulator still gets the chord first**: one that copies its own selection
  on `Cmd+C` and swallows the key when it has none never lets thurbox see it —
  and with thurbox holding the mouse, the emulator's selection is usually
  empty. Emulators that forward a shortcut they did not perform (Ghostty's
  `performable:` keybinds) pass it through; elsewhere, unmap the emulator's
  own `Cmd+C` (thurbox's copy writes the *system* clipboard, so nothing is
  lost by doing so), or use `Ctrl+C`.
- **Both pairs are ordinary bindings** (`kernel::clipboard`), listed in `F1`
  and rebindable — they were literal key arms in the loop, matched ahead of the
  registry, which is why help used to list them as *Fixed*.
- Any other keypress clears the selection.

Selection is highlighted in the terminal render buffer using
inverted colors.

### What gets copied

For a selection in the **terminal pane**, the text is read from the
session's vt100 grid rather than the painted cells
(`selection::extract_text_from_screen`). Two consequences:

- **Scrollback is selectable.** The grid resolves through the current
  scroll offset, so text you scrolled back to copies correctly.
- **Soft-wrapped lines are rejoined.** A line longer than the pane is
  stored as several rows with a wrap flag; those are one logical line,
  so they copy without a newline at the pane edge. A wrapped URL or
  code line pastes intact.

Trailing whitespace is trimmed per logical line; interior spacing is
preserved so column alignment survives. Other panes (session list,
info panel) have no grid behind them and are read from the frame
buffer as before.

### Copying over SSH (OSC 52)

Copy uses two transports, in order — configured by `[clipboard]
provider` in settings.toml (`auto` | `native` | `osc52` | `none`):

1. **Native** (`arboard`) — the local display server. Reports real
   success or failure, but only exists on the machine holding the
   clipboard. The handle is kept alive for the app lifetime to avoid
   Linux-specific "dropped too quickly" issues.
2. **OSC 52** — an escape sequence your *terminal emulator*
   interprets, so it reaches the clipboard of whoever is looking at
   the screen regardless of how many SSH hops are in between. Written
   to `/dev/tty` rather than stdout, because a multiplexer intercepts
   OSC 52 arriving on a child's stdout.

`auto` writes to **both**: native for the local case, OSC 52 for
whoever is actually looking at the screen. There is deliberately **no**
`$SSH_TTY` check — those vars are frequently stale under tmux (the
server daemonizes with its first client's environment), and Neovim
shipped SSH detection here and removed it in 0.11.

It used to *stop* at a successful native write, on the reasoning that
trying the local clipboard answers "is there one?" directly. That
holds only where a native clipboard is absent when nobody is at the
machine — X11 and Wayland, where a headless SSH session has no display
and `arboard` fails. Windows has no such property: the clipboard of a
session nobody is looking at accepts writes and reports success, so a
copy from a Windows host over SSH landed there, said `copied 15
line(s)`, and never reached the person who pressed the key. Writing
both costs one escape sequence a terminal either uses or ignores, and
removes the platform difference rather than adding a branch.

The toast names the transport only when OSC 52 was the *only* path that
ran (`copied 8 line(s) (OSC 52)`), so a terminal that silently ignores
the sequence is diagnosable while an ordinary copy stays quiet. Text
over ~74 KB skips OSC 52 — tmux discards an oversized sequence
**entirely** — and is an error only when the native write did not
carry it either.

thurbox sets `set-clipboard on` and `terminal-features ,*:clipboard`
on its own tmux server (`TmuxBackend::apply_clipboard_config`). Both
are required: tmux's default `set-clipboard external` **silently
discards** an OSC 52 originating inside a pane, and a missing `Ms`
terminfo capability drops it again at a second gate. Note the
tradeoff — `set-clipboard on` lets any process in a pane write your
system clipboard, which is why tmux's own default is more
conservative.

### Pasting over SSH

Paste never uses OSC 52. Terminals disable clipboard *reads* by
default (a remote host could exfiltrate your clipboard), and probing
for one can stall for seconds. When no local clipboard is reachable,
`Ctrl+V` shows a hint pointing at your terminal's own paste
(usually **`Ctrl+Shift+V`**), which delivers the text as an ordinary
bracketed paste that thurbox routes exactly like `Ctrl+V`.

### Pasting on Windows

A Windows terminal reports no paste at all — crossterm delivers one there as
ordinary key presses, so a multi-line prompt used to submit itself a line at a
time. thurbox rebuilds the paste from that key stream before it is dispatched,
by timing: characters arriving faster than anyone can type are gathered, and a
gathered run that carries a line break is handed over as one paste (so an agent
shows it as a paste, not as typing). Everything else is left exactly as it was
— every editing key passes straight through, ordinary typing is untouched, and
a line you type and submit with `Enter` still submits. Rationale: ADR-4 in
`docs/ARCHITECTURE.md`.

---

## Mouse Navigation

The whole TUI is clickable. Every list renderer reports the screen
rect of each row it draws; `App::view` records them per frame in a
click registry (`App::click_targets`, mirroring `scrollbar_hits`)
that the mouse handler hit-tests — first match wins, with rows
recorded before their pane's whole-rect focus fallback.

- **Click a row** (session list, tasks panel, automations pane,
  file viewer): selects it and focuses that pane. A session-list
  group header selects that group's first session. File rows also
  activate (toggle a directory, open a file in the editor).
  Clicking into another pane while an in-pane editor has unsaved
  edits discards them, exactly like `Esc`/`Ctrl+H`.
- **Click a pane**: focuses it; terminal and session-list clicks
  still arm drag-selection on the same press.
- **Click a picker row** (theme, agent, host, branch, task-action,
  automations list, restore, F1 editor): selects and confirms it in
  one click (Enter-equivalent — F1 starts chord capture). The repo
  picker is the exception: a row click toggles/folds (Space), since
  Enter there confirms the whole modal.
- **Clicks are swallowed by modals**: anywhere else on (or outside)
  an open modal does nothing — a stray click can never discard
  typed input or fall through to the panes beneath. Clicks are also
  ignored while the F1 editor is capturing a chord and while the
  global-search strip is open.
- **Hover**: the clickable row under the pointer is underlined
  (driven by mouse-move events; applied post-render from the same
  click registry).
- **One notch, one step**: a wheel *notch* is not one report. A
  terminal turns a detent into its line-scroll count — three, for
  ghostty, kitty and xterm — and under mouse reporting sends that
  many reports back to back, so one flick of the wheel used to walk
  the session list through three sessions and open each one on the
  way. Reports closer together than a person can turn a wheel
  (20 ms) are folded into the notch that started them, and a
  direction change always steps. A tick **forwarded to a pty** is
  deliberately left whole: there the three reports are the three
  lines the terminal means to scroll, and the program inside owns
  what they do.
- **Modal scrolling**: while a modal is open the wheel steps its
  selection (one row per notch, like `j`/`k`); overflowing picker
  lists window around the selection and draw a draggable scrollbar
  (`ScrollTarget::Modal`) in their rightmost column. Drag replays
  Up/Down through the modal's own key handler, so clamping and side
  effects (e.g. theme live preview) match keyboard navigation. Pane
  scrollbars beneath an overlay are never grabbable.

Dispatch order on click: modal (scrollbar grab → row act → swallow)
→ `Ctrl+Click` URL → pane scrollbar grab → global-search swallow →
click targets → text selection arming.

The whole subsystem is gated by `[features] mouse` in settings.toml
(default `true`): when disabled, mouse capture is never enabled, so
the terminal keeps its native mouse behavior.

### Click registry, pills, collapse chevron, and the central tab strip

One per-frame click-target registry backs every clickable surface — rows, footer
pills, modal buttons and fields, the collapse chevron, and the central-pane tabs:

Mouse clicks route through a per-frame registry (`App::click_targets`,
mirroring `scrollbar_hits`): list/modal renderers return `ui::RowHitbox`es,
`App::view` records them as `ClickAction`s, and `handle_mouse_click` hit-tests
them (rows select/confirm, panes focus, modals swallow everything else; the
hovered row is underlined via mouse-move events). **Clickable buttons** reuse
the registry: `ui::render_button_bar` draws filled "pill" buttons (` Label ` on
a solid accent/gray fill, no brackets) returning `ui::ButtonHit`es. The footer
renders Help/Info/Files/Theme/Tasks/Settings/Quit pills ordered by F-key
(`Help · F1` … `Settings · F6`) with `Quit` last, each suffixed with its live
(rebindable) shortcut (an F-key alternate where one exists, else the caret-ctrl
chord `Quit · ^Q`). Panel toggles are feature-gated (Info/Files/Tasks dropped
when their feature is off; dropped *together* when the footer can't fit the
full set — `pill_block_width` vs footer width — so Help/Theme/Settings/Quit
never fall off); with the file viewer open its hints fill the space to their
left. Pills are `ClickAction::Global(Action)` (a click runs `dispatch_action`,
ignored while a modal is open). Every modal footer renders action buttons
(Save/Cancel/Select/…) as `ui::ModalButtons` (each `ButtonHit` paired with the
key it replays), recorded as `ClickAction::ModalButton { code, mods }`;
`handle_modal_click` replays that key through the modal's own handler so a click
matches the keyboard path. **Clicking a field** selects it: editor modals
(Settings/Automation) ship per-field hitboxes as `ClickAction::ModalField(i)`
(→ `select_modal_field`, like Tab/↑↓) — and in **Settings** a click on a
boolean row also **toggles** it (scalar rows only select, so a stray click never
changes a number); the in-pane automation/task editors record
`ClickAction::PaneField { focus, index }` (→ focus + `select_pane_field`), and
the repo picker
`ClickAction::RepoFocus(..)` for its path-input/search sub-fields. Hovering a
button reverses its fill (`Modifier::REVERSED`), distinct from the row
underline. With a modal open the wheel steps its selection and overflowing
picker lists render a draggable scrollbar (`ScrollTarget::Modal`, drag replayed
as Up/Down through the modal's key handler). All gated by `[features] mouse` —
disabled, mouse capture is never enabled and the terminal keeps native mouse
behavior. `agent_picker_modal` drives the new-session flow.

- **Session-list collapse chevron.** A collapse/expand affordance toggles the
left session-list pane (`ToggleSessionList`, F9 — hides the list for a
full-width main pane). It sits at the **central pane's top-left border** in
*both* states — ` ◀ F9 ` while shown, ` ▶ F9 ` while hidden — so the control
that folds the list away also brings it back. It is deliberately **not** a pill
in the tab strip: those select a central *view* (one is always
accent-highlighted), whereas this is a binary pane-*visibility* toggle, and two
accent-filled pills conflated the two meanings. So it renders as an accent
chevron + muted F9 hint (bare chevron on a pane < 40 cols; suppressed on the
empty welcome screen).
`App::session_collapse_toggle_label` builds it, its hitbox is recorded as
`ClickAction::Global(ToggleSessionList)` **before** the pane's whole-rect focus
fallback (so the on-border click wins, sharing the F9 keypath), and the tab
strip packs to its right (`central_tab_cells(area, start_x)`);
`App::draw_session_collapse_toggle` paints it.
- **Central-pane tab strip.** The agent terminal, the per-session shell, and the
code-review view share the central pane, surfaced as a clickable tab strip
(`Agent · Review · F7 · Shell · F8`, packed right of the collapse chevron) on
the pane's **top border** by `App::draw_central_tabs`, each tab a filled **pill
button** (`ui::render_pill`, the standalone form of the footer's
`render_button_bar` chips) so it reads as clickable like the footer pills — the
active view accent-filled "primary", the rest neutral "secondary" (hover
reverses the fill via the shared `is_button` path). Each tab carries its
toggle's live shortcut hint, preferring the **F-key** alternate
(`tab_shortcut`) — a focused agent terminal passes `Ctrl+<letter>` through to
the CLI (`Ctrl+X` is emacs's prefix key, so it never reaches `ToggleReview`)
whereas the F-key dispatches in every pane; Agent has no dedicated key (the
Shell toggle returns to it), so it shows no hint. Shell/Review tabs are gated by
their feature flags. `central_tab_cells` lays out the on-border hitboxes
(recorded as `ClickAction::CentralTab(CentralTab::{Agent,Shell,Review})`
**before** the pane's whole-rect focus fallback so a tab click wins); a click
runs `App::select_central_tab`, which *selects* the view (closing any open
review when switching to Agent/Shell, opening it for Review) — distinct from the
keyboard `Ctrl+T`/`Ctrl+X` *toggles*. So the central pane's session-info title
(`terminal_view`/`code_review`) is **right-aligned** (via `title_top` +
`ui::title_style`) to leave the border's left free for the tabs. The F-keys
switch views from **any** view: `ToggleShell` is a `review_escape_chord` (so an
open review lets F8 fall through to the global binding instead of swallowing
it), and `toggle_shell_view` is review-aware — with a review open it closes it
and lands on the shell, mirroring the Shell tab.

**Giving the terminal back.** The escapes that turn reporting on are undone by
`restore_terminal` on every exit thurbox can see: a clean `Ctrl+Q`, a panic, and
— since the signal handler in `coordinator::boot` — a `SIGHUP`, `SIGTERM` or
`SIGINT`, which the process used to die on with the default action and no
cleanup, leaving the shell that came next printing `\x1b[<64;12;30M` on every
wheel notch (thurbox asks for `?1003`, so every pointer *move* reported too).
The exit status is the shell's `128 + signal`, so a wrapper can tell the two
apart. What no handler can fix is a **dropped ssh connection** to a remote
thurbox: the `?1003l` has no pty left to travel down, so the local emulator is
left reporting exactly as a killed remote `vim` leaves it on the alternate
screen. Type `reset` there (or `printf '\e[?1000l\e[?1003l\e[?1006l\e[?2004l\e[?1049l'`),
or run the ssh session inside a local tmux, which owns the outer terminal's
modes and puts them back on detach.

---

## Shell Pane Toggle

`Ctrl+T` (or `F8`) toggles between the agent session and a shell pane
(plain bash/zsh) for the active session. The shell runs in a
separate tmux pane alongside the agent pane.

Unlike the other readline-shadowing `Ctrl+<letter>` chords (`Ctrl+B`/`D`/`E`/
`F`/`O`/`P`/`R`/`S`/`U`/`W`), `Ctrl+T` is **not** passed through to the agent PTY
when a terminal is focused: it still toggles the shell. This is a deliberate
exception — readline's transpose-chars (`Ctrl+T`) is rarely used, and the
convenient shell toggle wins. `F8` is the equivalent alternate, matching the
other panel toggles' F-keys.

- **Status bar**: Shows "Shell" label when viewing the shell pane.
- **Per-session state**: Each session tracks its own `TerminalView`
  (Agent or Shell) independently.
- Input is forwarded to whichever pane is currently active.
- **Remote/WSL sessions**: the shell pane opens the host user's own
  interactive **login shell** — the same environment an `ssh <host>` login
  gives you (rc files, prompt, aliases, `PATH`), not a bare `/bin/sh`. It
  bootstraps through the always-present `/bin/sh -l` (which exports `$SHELL`)
  and then `exec "$SHELL" -l`, falling back to `/bin/sh -l` if `$SHELL` is
  unset. A psmux (Windows SSH) host keeps its native `powershell` pane.

---

## Clickable URLs

`Ctrl+Click` in the terminal pane opens the link under the cursor.
Two kinds resolve, and an **OSC 8 hyperlink wins** where both cover
a cell (`App::url_at_click`):

- **OSC 8 hyperlinks** — an agent renders a markdown link as
  `OSC 8 ; ; <url>` + label + `OSC 8 ; ;`, so the screen holds only
  the label (`Github`, never `https://github.com`) and the URL exists
  *solely* in the escape, which `vt100` discards. The parser callbacks
  capture each run instead (`agent::osc8` → `session::hyperlink`): the
  label is read off the screen between the cursor position at the open
  and the one at the close (the closing escape arrives after its label
  printed), and stored with its **start column**, not its row — the row
  moves every time the transcript scrolls, the printed glyphs and their
  column survive it. A click resolves only if that label is still on
  screen at that column, so a row whose content has moved on resolves
  to nothing rather than to a stale URL. A run the screen scrolled
  under mid-print is dropped for the same reason (agents redraw and
  re-emit the escape); a run long enough to wrap contributes one entry
  per row. The table is bounded at 512 runs per session.
- **Plain-text URLs** (`https://`, `http://`, `file://`) are scanned
  out of the rendered rows at click time (`kernel::terminal::links`). Trailing
  punctuation (`.`, `,`, `;`, `:`, `)`, `]`) is stripped.
  Display-width column offsets keep positioning correct on rows with
  wide (CJK/emoji) glyphs.

### Handing links back to the outer terminal

thurbox re-renders the agent's screen through ratatui, which has no
notion of a hyperlink — so the terminal **thurbox itself runs in** only
ever receives a plain label and can't offer its own open-link gesture.
That gesture matters: no escape sequence says "open this URL", so a
terminal-side click is the *only* way a thurbox on a remote host can open
a browser on the machine the user is sitting at.

So after each frame is flushed, `App::paint_outer_hyperlinks`
re-prints the visible runs wrapped in OSC 8 — the same glyphs with the
same styles, read back out of the drawn frame, so nothing changes
visually and only the terminal's hyperlink state is added. Windows
Terminal, kitty, WezTerm and iTerm2 then underline the label on hover and
open the user's own browser on Ctrl/Cmd+Click.

**An interface pane rides the same pass**, via the `url:<link>` click verb
(`ClickVerb::Url`). It has to: a plugin returns cells and the kernel paints
them, so nothing in that path can put an escape on the wire — the identical
text in a pane was not Ctrl+Click-able while an agent's transcript was, and
on a remote host the outer terminal is the only leg with a browser to reach.
The verb's nodes are read out of the drawn frame like a session's runs, and
`docs/PLUGINS.md` has the authoring side.

Three properties keep this safe and cheap:

- **Bracketed in DECSC/DECRC**: the draw places the caret last and leaves it
  *shown*, so re-printing walks it away — a focused text field's caret was left
  wherever the final run ended, and the forced-redraw floor put it back and took
  it away again several times a second. That reads as a cursor blinking in the
  wrong place, which is why it looked like a rendering fault rather than a moved
  cursor. The pass saves and restores the position itself instead of leaving it
  to the next `draw`: any number of frames may pass before that one, and every
  one of them is a frame with a stray caret.
- **Validated against what was drawn** (`helpers::drawn_label_cells`): a
  candidate is emitted only if the frame's cells still print that label
  there, so a covering overlay, a scrolled pane, or a repainted row
  yields nothing instead of escapes written over current content. A label
  clipped by the pane's right edge is linked as far as it is visible.
  A `url:` node has no label to match — its text lives in the plugin's
  tree, already through wrapping, alignment and scroll — so the covering
  surfaces are checked directly instead (`App::link_paint_obscured`: a
  modal owns the screen, a float owns its rect). Without it a modal over
  such a node would link the modal's own glyphs. Blank cells either side
  of the node's glyphs are trimmed, since the rect a node is given is
  wider than the text in it.
- **Off the hot path when unused**: the pass bails on
  `HyperlinkTable::is_empty()` before computing layout or scanning the
  screen, so a session whose agent never printed a link pays one check
  per frame. Only the newest `VISIBLE_SCAN_LIMIT` (128) runs are scanned.

The URL is stripped of control characters before it goes out
(`hyperlink::osc8_open`): it is agent-controlled text being written back
to the user's terminal, and an embedded `ESC` would end the sequence
early and let the rest be interpreted as escapes of its own.

**Caveat:** while thurbox has mouse capture on (`[features] mouse`), a
terminal that forwards Ctrl+Click to the application instead of handling
its own hyperlink will land on thurbox's own click path (which opens, or
falls back to copying, per below). Setting `mouse = false` gives all
clicks back to the terminal.

### Where the URL goes

The click **always toasts its outcome**, so a resolved link that
couldn't be acted on is never indistinguishable from a click on plain
text (it used to be: the opener was spawned with its result discarded).

`helpers::open_url` hands the URL to the platform opener — `open` on
macOS, `cmd /C start` on Windows, `xdg-open` elsewhere. On Linux/BSD it
first checks there is something to open *into* (`DISPLAY`,
`WAYLAND_DISPLAY`, or a `BROWSER` the user set): a thurbox running on a
headless or SSH host has none, where spawning `xdg-open` either fails or
— worse — succeeds and does nothing.

With no browser reachable the URL goes to the **clipboard** instead
(`App::write_clipboard`, the same native → OSC 52 path `Ctrl+C` uses).
That is what makes the feature work over SSH at all: the OSC 52 leg
travels to the terminal the user is sitting at, so the URL lands in
*their* clipboard, ready to paste into a real browser. The toast names
the route (`(OSC 52)`) so a terminal that drops the sequence is
diagnosable.

---

## Planned Features

Directional intent, not commitments. These may change as the
project evolves.

- **Multi-session orchestration**: Broadcast input to multiple
  agent sessions simultaneously.
- **Task delegation**: Split a task across multiple sessions with
  dependency tracking.
