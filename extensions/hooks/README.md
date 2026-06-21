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
  opencode is installed). Events: `chat.message` → working, `permission.asked`
  → blocked, `session.idle` → done.
- **codex** — codex's stable `notify` program runs on **agent-turn-complete**,
  which maps to **done**. It's injected as a launch `-c notify=…` override (an
  `[[agent_patches]]`), so your `~/.codex/config.toml` is never written and the
  wiring is fully reversible. codex has no per-event notify, so working/blocked/
  idle aren't expressible this way — the dot is accurate for the turn-complete
  edge (complementary to aider's blocked-only).
- **gemini** *(experimental)* — gemini loads hooks only from its shared
  `~/.gemini/settings.json`, so we **JSON-merge** our entries in (a
  `[[config_merges]]`, guarded by `requires_dir`) without clobbering your
  settings; uninstall prunes exactly ours back out. Events: `SessionStart` →
  idle, `BeforeTool` → working, `AfterAgent` → done. **No blocked** — gemini has
  no permission-only event (only an ambiguous `Notification` that also fires on
  idle, the false-red trap we avoid for claude). **Caveats:** the event
  names/settings shape are doc-sourced and unverified against a live binary — if
  they differ, edit `gemini-hooks.json` (no code change). And if gemini sanitizes
  the hook environment, `$THURBOX_SESSION` may not reach the hook, in which case
  the signal is a fail-open no-op. Validate end-to-end before relying on it.

## Mechanism

This extension exercises two extension-manifest capabilities (see
`src/session/extension_def.rs`):

- `[[agent_patches]]` — append args to an **existing** agent in `agents.toml`
  (reversible; uninstall removes exactly the injected subsequence).
- `[[external_files]]` — place a file into an agent's **own** config dir
  (outside the extension home), guarded by `requires_dir`.
- `[[config_merges]]` — **reversibly deep-merge** shipped JSON into an agent's
  own *shared* config file (gemini's `settings.json`) without clobbering the
  user's other settings: objects recurse, arrays union, and uninstall prunes
  exactly the entries we shipped (matched by the `session signal` marker, so it
  stays correct across payload changes). Guarded by `requires_dir`; no-op when
  the merge is already present. A merge whose target is malformed JSON is
  soft-skipped (logged, never aborts the rest of the install).

  Note: on the **first** merge, thurbox rewrites `settings.json` with normalized
  formatting (alphabetized keys, 2-space indent). This is one-time and lossless —
  your values are untouched and the file is stable afterward.
