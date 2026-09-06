# hooks — agent lifecycle → thurbox session status

The **hooks** extension wires each coding agent's lifecycle hooks to
`thurbox-cli session signal` so every session reports its state back to thurbox
and the sidebar shows, at a glance, which agents are **blocked**, **working**,
or **done**:

| State | Colour | Meaning |
|-------|--------|---------|
| 🔴 blocked | red | the agent needs input or approval |
| 🟡 working | yellow | the agent is actively running |
| 🔵 done | blue | a turn just finished; shown until you switch away |
| 🟢 idle | green | acknowledged (you moved off it), or at rest |

Repo groups in the session list roll up to their most-urgent member, so the
whole list scans in one pass.

## It's on by default

Unlike most extensions, **hooks ships built into thurbox and is auto-activated**
on first run — the default agent's hook is pre-configured with zero setup.
(`ui-skill` is the other one built in this way.) Opt out at any time:

```bash
thurbox-cli extension deactivate hooks   # remove the wiring; won't come back
thurbox-cli extension activate hooks      # re-enable it
```

## How each agent is wired

The hook command is always `thurbox-cli session signal --state <working|blocked|done>`,
which identifies the calling session from the injected `$THURBOX_SESSION` (no ids
passed by hand) and is suffixed `|| true` so it can never break the agent.

- **claude** — a managed settings file (under the extension home) is passed via
  `--settings` (an `[[agent_patches]]` that appends the flag to the built-in
  `claude` agent, reversibly). claude merges it with your own settings, so your
  hooks are preserved. Events: `SessionStart` → idle (so a just-booted, idle
  session isn't shown as working), `UserPromptSubmit`/`PreToolUse`/`PostToolUse`
  → working, `Stop` → done. `Notification` → blocked **only for
  permission/approval prompts** — claude also fires `Notification` for its
  "waiting for your input" idle nudge, which the hook ignores (parses the
  payload) so an idle session doesn't flip to red. `PostToolUse` is what clears
  that block: claude has no "permission granted" event, so the tool finishing is
  the first thing it reports after the prompt is answered — without it an
  approved session stayed red until the *next* tool call, or until `Stop` if the
  turn had no more.
- **aider** — `--notifications-command` reports the only edge aider exposes:
  blocked (waiting for input).
- **opencode** — a plugin dropped into `~/.config/opencode/plugin/` (only when
  opencode is installed). Events: `session.created` → idle, `chat.message` →
  working, `permission.asked` → blocked, `permission.replied` → working
  (allowed or denied, the turn is opencode's again — it is the only agent here
  with a real permission-reply event), `session.idle` → done.
- **codex** *(experimental)* — codex's `hooks.json` is claude-shaped, loaded
  from `~/.codex/hooks.json`. We **JSON-merge** our entries in (a
  `[[config_merges]]`, guarded by `requires_dir`) so your own hooks are
  preserved; uninstall prunes exactly ours back out. Events: `SessionStart` →
  idle, `UserPromptSubmit`/`PreToolUse` → working, `Stop` → done. **No blocked**
  — codex's top-level hooks have no permission/approval event (that lives only in
  the legacy `notify`). This replaced the old `-c notify=…` override (which only
  reported done); the trade is a reversible write into a separate
  `~/.codex/hooks.json`, never your `config.toml`. **Caveat:** codex's hooks.json
  is newer than its `notify`; the event names are assumed identical to claude's —
  if they differ, edit `codex-hooks.json` (no code change).
- **vibe** *(experimental)* — Mistral Vibe loads hooks from `~/.vibe/hooks.toml`.
  It's TOML, so we can't JSON-merge it — we drop a managed file in (an
  `[[external_files]]`, guarded by `requires_dir`, only when vibe is installed).
  Verified against vibe 2.21.0 (`vibe.core.hooks.models.HookConfig`): each entry
  needs `name` + `type` (`pre_tool`/`post_tool`/`post_agent`) + `command`. Events:
  `pre_tool` → working, `post_agent` → done. **No blocked** — vibe's only hook
  types are pre_tool/post_tool/post_agent (no permission/notification event),
  so a tool awaiting approval reads as `working` (`pre_tool` fires *before* the
  approval prompt). If a future vibe renames the types/fields, edit
  `vibe-hooks.toml` (no code change). And if you already maintain your own
  `~/.vibe/hooks.toml`, the write is **refused** (no managed marker) so it's never
  clobbered — vibe simply goes unreported rather than broken.
- **copilot** *(experimental)* — GitHub Copilot CLI (the `copilot` command) loads
  hooks from its own dir, `~/.copilot/hooks/*.json`. We drop a managed standalone
  file in (an `[[external_files]]`, guarded by `requires_dir`, only when copilot is
  installed), so your other hook files are never touched. Events (copilot's own
  schema): `sessionStart` → idle,
  `userPromptSubmitted`/`preToolUse`/`postToolUse` → working, `agentStop` → done,
  and `notification` matched to `permission_prompt` → blocked (so an
  `agent_idle`/`shell_completed` notification doesn't flip the dot red).
  `postToolUse` is what clears that block — copilot has no "permission granted"
  event, so the tool completing is the first thing it reports once the prompt is
  answered.
  Both `bash` and `powershell` commands are shipped, so status works on Windows
  too. **Caveat:** if a future `copilot` changes the hook schema, edit
  `copilot-hooks.json` (no code change).
- **grok** *(experimental)* — xAI's Grok Build CLI loads every `*.json` in
  `~/.grok/hooks/` on its own, so we drop a managed standalone file in (an
  `[[external_files]]`, guarded by `requires_dir`, only when grok is installed)
  and never touch a hook file you wrote. It is the **global** dir on purpose:
  those hooks are always trusted and load on first launch, while
  `<project>/.grok/hooks` additionally needs the folder granted trust in grok's
  own `~/.grok/trusted_folders.toml`. grok is Claude-Code-compatible, so the
  mapping mirrors claude: `SessionStart` → idle,
  `UserPromptSubmit`/`PreToolUse`/`PostToolUse` → working, `Notification` →
  blocked **only for permission/approval prompts** (the payload is matched, so
  an idle nudge doesn't flip the dot red), `Stop` → done. Every command here is
  `$`-free: grok silently refuses to load a whole hook file whose command
  references `$VAR` without an inline `:-default`, so the blocked edge pipes
  stdin through `grep` rather than reusing claude's `case "$(cat)"`. **Caveat:**
  if a future grok renames its events, edit `grok-hooks.json` (no code change).
- **kimi** *(experimental)* — Kimi Code CLI reads hooks from a `[[hooks]]` array
  in `~/.kimi-code/config.toml`. That one file is your whole kimi configuration
  and there is no drop-in hooks dir, so a managed file would clobber it — we
  **merge** our entries in instead (a `[[config_merges]]` with `format = "toml"`,
  guarded by `requires_dir`); `toml_edit` keeps your comments and key order, and
  uninstall prunes exactly ours back out. Events: `SessionStart` → idle,
  `UserPromptSubmit`/`PreToolUse`/`PostToolUse` → working, `PermissionRequest` →
  blocked, `PermissionResult` → working, `Stop` → done. This is the one agent
  here whose block edge is structured on **both** sides — a real permission
  request and a real permission result — so `blocked` neither false-fires on an
  idle notification nor latches until the next tool call. **Caveat:** kimi
  accepts exactly four keys per hook entry (`event`/`command`/`matcher`/
  `timeout`) and refuses to load the entire config file if it sees a fifth, so
  `kimi-hooks.toml` must never grow one.
- **antigravity** — antigravity (the `agy` CLI, the Gemini CLI successor) loads
  hooks only from its shared `~/.gemini/settings.json`, so we **JSON-merge** our
  entries in (a `[[config_merges]]`, guarded by `requires_dir`) without clobbering
  your settings; uninstall prunes exactly ours back out. `agy` adopted Claude
  Code's hook schema (verified against agy 1.0.9), so the mapping mirrors claude:
  `SessionStart` → idle, `PreToolUse`/`PostToolUse` → working, `Stop` → done, and
  `Notification` → blocked **only for permission/approval prompts** (the payload
  is parsed, same as claude, so an idle `Notification` doesn't flip the dot red);
  `PostToolUse` clears the block, for claude's reason. It has no
  `UserPromptSubmit`, so working is signaled at the first tool call rather than on
  prompt submit. **Caveat:** if agy sanitizes the hook environment,
  `$THURBOX_SESSION` may not reach the hook, in which case the signal is a
  fail-open no-op. If a future `agy` changes the hook schema, edit
  `antigravity-hooks.json` (no code change).
- **pi** *(experimental)* — the pi.dev CLI auto-discovers TypeScript extensions
  from `~/.pi/agent/extensions/*.ts`, so we drop a managed extension in (an
  `[[external_files]]`, guarded by `requires_dir`, only when pi is installed). It
  subscribes to pi's lifecycle events: `session_start` → idle, `agent_start`,
  `tool_execution_start` and `tool_execution_end` → working, `agent_end` → done,
  and a tool call to `ask_user_question` → blocked. There is no "answered"
  event, so `tool_execution_end` is what clears that block: the question tool
  finishing *is* the answer arriving. **Caveats:** pi has no claude-style
  `Stop`/permission hook, so `blocked` is inferred only from a structured
  `ask_user_question` tool call — a turn that ends by asking something in prose
  signals `done`, not `blocked`. If you already maintain your own file at that
  path the write is **refused** (no managed marker), so it's never clobbered — pi
  simply goes unreported rather than broken. Remote (SSH/WSL) pi sessions are
  provisioned like the other config-dir agents (the rewritten payload ships
  into the host's extensions dir); a psmux/Windows host shows `Hooks: degraded`.
  If a future `pi` renames its events, edit `pi-status.ts` (no code change).
- **omp** *(experimental)* — Oh My Pi (`omp`) is Pi-compatible and likewise
  auto-discovers TypeScript extensions, from `~/.omp/agent/extensions/*.ts`, so
  we drop a managed extension in (an `[[external_files]]`, guarded by
  `requires_dir`, only when omp is installed). It mirrors pi's status extension
  but maps OMP's structured user-question tool — named `ask` — to `blocked`;
  it recognizes **both** `ask` and pi's `ask_user_question`, so reusing pi's
  file unchanged would leave OMP stuck `working` while it waits. Events:
  `session_start` → idle, `agent_start`/`tool_execution_start`/
  `tool_execution_end` → working (`ask`/`ask_user_question` → blocked, cleared
  by that tool ending), `agent_end` → done. Same caveats as pi
  (managed-marker refusal, remote provisioning, psmux `Hooks: degraded`).
  Verified against OMP 17.0.6; if a future `omp` renames its events, edit
  `omp-status.ts` (no code change).

## Agents this extension does not wire

Two agents have a hook surface none of the three mechanisms can reach, so they
are left out rather than given a payload that never fires — `session get` keeps
saying `hook_coverage: "none"`, which is the truth:

- **cursor** (`cursor-agent`) — the only scope its CLI is known to load
  `stop`/`sessionStart` from is per-project `<repo>/.cursor/hooks.json`, and only
  when launched with `--trust`. That is a per-repo file plus a launch flag, so it
  would cover nothing an outside driver starts. User-scope `~/.cursor/hooks.json`
  is documented for the IDE; in the CLI it is reported to run only the
  shell/MCP/file-edit hooks — none of which can say `done`, so wiring it there
  would latch a session at `working` forever.
- **muse** (Muse Code) — its hooks exist only as capabilities of a native plugin
  that must be installed *and approved* with `muse plugins install` / `muse
  plugins approve`; a dropped-in config file is silently ignored. An extension
  manifest writes files, it does not run an agent's commands.

## Custom agents (`hook_schema`)

The wiring above is keyed to the built-in agent **names**, so a **custom** agent
you add to `agents.toml` (e.g. a rebranded-claude `fleet`) normally gets no
hooks — its status dot stays driven only by the agent-neutral fallbacks (working
inferred from output, done from output quiescence). To opt a custom agent into a
known family, set `hook_schema` on its `[[agents]]` entry:

```toml
[[agents]]
name = "fleet"
command = "fleet"        # runs claude under the hood
hook_schema = "claude"   # ⇒ inherit claude's --settings hook wiring
```

thurbox then applies the `claude` `[[agent_patches]]` to `fleet` as well, so it
reports working/blocked/done exactly like `claude` (locally and on a remote/WSL
host). `hook_schema` names the *family* to imitate; today `"claude"` is the
useful value — it's the family wired via a per-agent arg patch. The
config-dir-wired families (codex/opencode/antigravity/vibe/copilot/grok/kimi)
don't need it: a rebrand that runs the same CLI reads the same `~/.<agent>/…`
hook file and already reports.

**grok and kimi are wired without being built-in agents.** thurbox ships no
`agents.toml` entry for either, but their payloads land in their own config dirs
all the same — so a grok or kimi started from a `--command` shell, from your own
`agents.toml` entry, or by an outside driver reports state like any built-in.
Tell thurbox which agent the pane is really running so the row resolves that
coverage:

```bash
thurbox-cli session create --name x --repo-path … --agent shell --reports-as grok
thurbox-cli session reports-as <ref> kimi     # or --clear to take it back
```

## Where the config lives

The wiring is applied **only to agents thurbox launches** — it never edits your
own global agent config (e.g. your personal `~/.claude/settings.json`). For
claude the managed hooks file is passed with `--settings`, which claude **merges
on top of** your own settings: inside a thurbox session both your hooks and
thurbox's fire, while a plain `claude` outside thurbox sees only your own. The
other agents are wired by a reversible merge into — or a managed file dropped in
— their own config dir.

| Agent | On-disk location | How it's applied |
|-------|------------------|------------------|
| claude | `~/.config/thurbox/hooks/claude.json` | `--settings` flag on the `claude` agent (claude merges it) |
| aider | — (no file) | `--notifications-command` flag on the `aider` agent |
| opencode | `~/.config/opencode/plugin/thurbox-status.js` | managed plugin file (`requires_dir`) |
| codex | `~/.codex/hooks.json` | reversible JSON-merge of our entries |
| vibe | `~/.vibe/hooks.toml` | managed file (refused if you already have one) |
| copilot | `~/.copilot/hooks/thurbox-status.json` | managed standalone file (`requires_dir`) |
| antigravity | `~/.gemini/settings.json` | reversible JSON-merge of our entries |
| pi | `~/.pi/agent/extensions/thurbox-status.ts` | managed extension file (`requires_dir`) |
| grok | `~/.grok/hooks/thurbox-status.json` | managed standalone file (`requires_dir`) |
| kimi | `~/.kimi-code/config.toml` | reversible TOML-merge of our entries |
| omp | `~/.omp/agent/extensions/thurbox-status.ts` | managed extension file (`requires_dir`) |

The home dir is `~/.config/thurbox/hooks` for a release build and
`~/.config/thurbox-dev/hooks` for a dev build, so the two stay isolated.

**Inspect or customize.** To see exactly what thurbox installed, read the file
for the agent above (e.g. `cat ~/.config/thurbox/hooks/claude.json`). The
injected `--settings` / `--notifications-command` flags themselves live in the
`claude` / `aider` entries of `~/.config/thurbox/agents.toml`. You can hand-edit
a managed file, but self-heal rewrites it from the embedded payload on the next
TUI start / heartbeat tick — so to keep a change, either deactivate the extension
(`thurbox-cli extension deactivate hooks`) and wire the hook yourself, or edit
the payload source under `extensions/hooks/` and reinstall.

## Checking that it fires

Every hook command ends in `|| true` on purpose — a missing `thurbox-cli`, a
locked database, or a hook firing outside a thurbox session must never break the
agent. The cost is that a signal which never lands looks exactly like an agent
that simply has not signalled yet:

```bash
thurbox-cli session doctor          # every active session
thurbox-cli session doctor <uuid>   # just one
```

It reports whether this extension is active, what the session's agent can report
at all, whether its payload is really on disk where the agent reads it, whether
a hook command could resolve `thurbox-cli` on `PATH`, what was last reported and
how long ago, and whether the pane's foreground process agrees. It exits
non-zero when a session's wiring is broken (an uncovered agent that is
signalling anyway warns rather than fails), and only ever reads —
`thurbox-cli extension reinstall hooks` is the repair.

## Reporting state for an agent thurbox did not launch

These hooks are wired at **launch**, for an agent thurbox knows from
`agents.toml`. A harness that owns the agent launch itself — asking thurbox for
a bare interactive shell and starting the agent inside that pane — gets none of
them. It can still report state, because `THURBOX_SESSION` is set on the pane
and inherited by every process in it:

```bash
thurbox-cli session signal --state working   # identity from $THURBOX_SESSION
thurbox-cli session signal --state done
```

Point your own agent's lifecycle hooks at that and the session reports exactly
like a built-in. Failing even that, thurbox reads the pane: a session that never
signalled but whose foreground process is an agent your `agents.toml` knows
reports `state: "running"` with `state_source: "process"` — coarser than a hook
by design, but not silence.

## Mechanism

This extension exercises two extension-manifest capabilities (see
`src/session/extension_def.rs`):

- `[[agent_patches]]` — append args to an **existing** agent in `agents.toml`
  (reversible; uninstall removes exactly the injected subsequence).
- `[[external_files]]` — place a file into an agent's **own** config dir
  (outside the extension home), guarded by `requires_dir`.
- `[[config_merges]]` — **reversibly deep-merge** a shipped document into an
  agent's own *shared* config file (antigravity's `~/.gemini/settings.json`,
  kimi's `~/.kimi-code/config.toml`) without clobbering the
  user's other settings: objects/tables recurse, arrays union, and uninstall prunes
  exactly the entries we shipped. JSON recognises them by the `session signal`
  marker in their content; TOML by an ownership comment stamped on each entry,
  which is stricter in both directions — a hook *you* wrote that calls `session
  signal` is not ours and survives uninstall, and an entry of ours whose event or
  command changed in a later payload is still ours and gets replaced rather than
  duplicated. Guarded by `requires_dir`; no-op when
  the merge is already present. A merge whose target is malformed is
  soft-skipped (logged, never aborts the rest of the install). JSON by default;
  `format = "toml"` picks the TOML merge (`agent::toml_merge`, on `toml_edit`,
  so the user's comments and key order survive).

  Note: on the **first** merge, thurbox rewrites `settings.json` with normalized
  formatting (alphabetized keys, 2-space indent). This is one-time and lossless —
  your values are untouched and the file is stable afterward.

## Remote (SSH/WSL) sessions

`thurbox-cli` isn't installed on a remote host, so the shipped hook commands
are rewritten there to set a tmux **pane user option**
(`tmux set-option -p @thurbox_state <s>`) that the local TUI picks up over its
control-mode connection. Delivery per agent, at spawn time:

- **claude** — the `--settings` hooks file is copied to the host (rewritten)
  and the arg substituted.
- **aider** — its literal `--notifications-command` arg is rewritten in place.
- **codex / antigravity / opencode / vibe / copilot / grok / kimi** — the rewritten payload
  is provisioned into the host's agent config dir
  (`session_ops::remote_hooks`), with the same safety rules as the local
  install: skipped when the agent isn't installed there (`requires_dir` probed
  over ssh), deep-merge-not-clobber for a shared config (prune-then-merge so
  upgrades replace rather than accumulate — JSON on either the `session
  signal` or `@thurbox_state` marker in an entry's content, TOML on the same
  ownership comment used locally, which recognises a stale entry under either
  command form), managed-marker guard for standalone files, and
  compare-before-write.

The local TUI receives the state over its persistent control-mode connection;
with the TUI closed, the headless `automation tick` (the 60 s tmux heartbeat)
polls hosts that have live remote sessions and writes changes to the same
database columns, so remote status keeps flowing either way.

Provisioning is **best-effort** (a down host or refused write degrades to a
`Hooks: degraded` hint in the info panel — never a failed spawn) and
**one-way**: thurbox never uninstalls from remote hosts (same policy as remote
worktrees). The files it leaves carry both prune markers, so removing them by
hand — or a future remote prune — needs no schema knowledge. Windows (`psmux`)
hosts are not provisioned yet (gated on `session::psmux_hook_rewrite_supported`).
