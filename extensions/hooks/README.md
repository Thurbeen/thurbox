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

Unlike other extensions, **hooks ships built into thurbox and is auto-activated**
on first run — the default agent's hook is pre-configured with zero setup. Opt
out at any time:

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
  session isn't shown as working), `UserPromptSubmit`/`PreToolUse` → working,
  `Stop` → done. `Notification` → blocked **only for permission/approval
  prompts** — claude also fires `Notification` for its "waiting for your input"
  idle nudge, which the hook ignores (parses the payload) so an idle session
  doesn't flip to red.
- **aider** — `--notifications-command` reports the only edge aider exposes:
  blocked (waiting for input).
- **opencode** — a plugin dropped into `~/.config/opencode/plugin/` (only when
  opencode is installed). Events: `session.created` → idle, `chat.message` →
  working, `permission.asked` → blocked, `session.idle` → done.
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
  Events: `before_tool` → working, `after_turn` → done, `notification` (awaiting
  approval / question) → blocked. **Caveats:** the `hooks.toml` schema is not
  verified against the live `vibe` binary — if event/key names differ, edit
  `vibe-hooks.toml` (no code change). And if you already maintain your own
  `~/.vibe/hooks.toml`, the write is **refused** (no managed marker) so it's never
  clobbered — vibe simply goes unreported rather than broken.
- **antigravity** — antigravity (the `agy` CLI, the Gemini CLI successor) loads
  hooks only from its shared `~/.gemini/settings.json`, so we **JSON-merge** our
  entries in (a `[[config_merges]]`, guarded by `requires_dir`) without clobbering
  your settings; uninstall prunes exactly ours back out. `agy` adopted Claude
  Code's hook schema (verified against agy 1.0.9), so the mapping mirrors claude:
  `SessionStart` → idle, `PreToolUse` → working, `Stop` → done, and `Notification`
  → blocked **only for permission/approval prompts** (the payload is parsed, same
  as claude, so an idle `Notification` doesn't flip the dot red). It has no
  `UserPromptSubmit`, so working is signaled at the first tool call rather than on
  prompt submit. **Caveat:** if agy sanitizes the hook environment,
  `$THURBOX_SESSION` may not reach the hook, in which case the signal is a
  fail-open no-op. If a future `agy` changes the hook schema, edit
  `antigravity-hooks.json` (no code change).

## Mechanism

This extension exercises two extension-manifest capabilities (see
`src/session/extension_def.rs`):

- `[[agent_patches]]` — append args to an **existing** agent in `agents.toml`
  (reversible; uninstall removes exactly the injected subsequence).
- `[[external_files]]` — place a file into an agent's **own** config dir
  (outside the extension home), guarded by `requires_dir`.
- `[[config_merges]]` — **reversibly deep-merge** shipped JSON into an agent's
  own *shared* config file (antigravity's `~/.gemini/settings.json`) without clobbering the
  user's other settings: objects recurse, arrays union, and uninstall prunes
  exactly the entries we shipped (matched by the `session signal` marker, so it
  stays correct across payload changes). Guarded by `requires_dir`; no-op when
  the merge is already present. A merge whose target is malformed JSON is
  soft-skipped (logged, never aborts the rest of the install).

  Note: on the **first** merge, thurbox rewrites `settings.json` with normalized
  formatting (alphabetized keys, 2-space indent). This is one-time and lossless —
  your values are untouched and the file is stable afterward.
