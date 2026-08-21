# Proposal: Landing-box operator for one-shot session spawn

**Status:** draft — implementation not yet greenlit
**Author:** <thomas@spotpay.us>
**Related:** `docs/FEATURES.md` (operator/coder/debugger roles), `docs/MCP_ROLES.md`, `~/dev/skills/thurbox/.claude/skills/start-session/SKILL.md`

## Problem

Thurbox has three "operator-shaped" slots (operator, debugger, dispatcher). In
practice one of those slots is spent as a scratchpad for **new work
initiation** — the user types a short prompt ("investigate sentry 733…", "add
a bulk-transfer endpoint"), the operator classifies it and spawns a dedicated
worktree session via `/start-session`, and then the operator's context is
polluted with routing metadata that has no long-term value. A week in, that
operator's transcript is mostly spawn receipts and the user has to `/clear` it
anyway.

The desired shape is **ChatGPT's landing input**: an always-available textbox
whose sole purpose is "turn this sentence into a fresh session." Zero
accumulated state. The user's mental model becomes:

- **operator #1** — accumulated ops/customer state (real long-lived work)
- **operator #2** — accumulated triage/investigation state (real long-lived work)
- **landing box** — fire-and-forget spawn input, no history retained

## Prior art in this repo

The primitives already exist. This proposal is about assembling them.

### `thurbox-cli session create --json` — the spawn CLI

Synchronous, returns the new session UUID. Called by the `/start-session`
skill today; it is the "non-interactive handle" the ask referenced.

```bash
thurbox-cli session create \
  --name landing-2026-08-21-abc \
  --repo-path /Users/tch/code/spotpay/backend \
  --agent claude-coder \
  --worktree-branch coder-bulk-transfer \
  --base-branch main \
  --json
```

By the time it returns, the worktree exists, the tmux window is live, and the
role wrapper (`~/.local/bin/thurbox-role-<role>`) has finished MCP setup
(`scripts/role.sh <role>`) and launched claude. No idle-wait required — the
`/start-session` skill then pipes the prompt in via `tmux paste-buffer -p`.

### `/start-session` skill — classification + naming + spawn

At `~/dev/skills/thurbox/.claude/skills/start-session/SKILL.md`. Already does
everything the landing box needs:

- **Role classification** from prompt keywords (sentry/CI/crash → `debugger`;
  feature/refactor → `coder`; admin lookup → `operator`).
- **Worktree name derivation** (short kebab-case, role-prefixed).
- **Repo defaulting**: `git rev-parse --path-format=absolute --git-common-dir`
  → main repo, so if the landing session lives in
  `/Users/tch/code/spotpay/backend` it will spawn backend worktrees by
  default. No new alias config needed.
- **Prompt injection** via `tmux -L thurbox paste-buffer -p` (bracketed
  paste), followed by a separate `send-keys Enter`.
- **Suffix appending**: PR-creation instructions + self-monitor block.
- **Hard rule** in the skill header: *"ALWAYS SPAWN, NEVER DO THE WORK
  YOURSELF."*

### Role layering

- `~/.config/thurbox/agents.toml` — `[[agents]]` entry per role, pointing
  `command = "thurbox-role-<role>"`.
- `~/.local/bin/thurbox-role-<role>` — a symlink to the shared wrapper. The
  wrapper reads `$0` to pick model + permission mode, runs
  `scripts/role.sh <role>` in the current worktree (which templates
  `.claude/roles/<role>.mcp.json` into `.mcp.json`), then `exec`s claude with
  `--add-dir ~/dev/skills/thurbox/` so the `/start-session` skill is in
  scope.
- `.claude/roles/<role>.mcp.json` in the target repo — which MCP servers this
  role gets.

## Design options

### (a) A new `landing` role

- New file `.claude/roles/landing.mcp.json` in spotpay/backend (empty
  `{"mcpServers": {}}` — the landing box needs no MCP servers, only bash +
  the `/start-session` skill).
- New wrapper symlink `~/.local/bin/thurbox-role-landing`.
- New `[[agents]]` entry `claude-landing` in `~/.config/thurbox/agents.toml`.
- A CLAUDE.md fragment (loaded via `.claude/settings.json` `additionalDirectories`
  or committed into `.claude/roles/landing.md`, loaded by the wrapper via
  `--append-system-prompt` — see below) that hard-codes the fire-and-forget
  behaviour:

  > On every user message: your only action is to call the `/start-session`
  > skill with the message verbatim as the prompt. Do not perform, plan, or
  > read anything related to the task. Do not answer questions. Do not
  > acknowledge except to report the spawn UUID.
  >
  > **Escape hatch:** if the message begins with `!keep`, do NOT spawn — treat
  > it as a normal chat message.

**Pros:**

- Data-driven. No thurbox core changes. Ships this week.
- Uses the same layering that already governs operator/coder/debugger.
- The `/start-session` skill's own classifier picks the downstream role — so
  the landing box doesn't need its own classifier.
- Repo defaulting is inherited for free (`/start-session` reads
  `git rev-parse` in the landing session's cwd → backend).

**Cons:**

- The "only ever spawn, never do the work" rule is enforced only by prompt.
  The model can drift, especially on ambiguous prompts. Mitigated by (i) the
  `/start-session` skill's own "ALWAYS SPAWN" warning, which reinforces the
  role rule, and (ii) the `Stop`-hook /clear defense below, which makes
  drift *cheap* — even if the model does one wrong turn, the transcript is
  wiped afterwards.

### (b) A `UserPromptSubmit` hook that short-circuits the model

`.claude/hooks/landing-spawn.sh` on this session only, wired into
`.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/landing-spawn.sh" }] }
    ]
  }
}
```

The hook receives the prompt on stdin, calls `thurbox-cli session create`
directly, sends the prompt to the new session via `tmux paste-buffer`, and
emits a `UserPromptSubmit` `hookSpecificOutput` with
`permissionDecision: "deny"` (and `permissionDecisionReason: "spawned
session <uuid>"`) to block the prompt from ever reaching the model.
`/clear` is unnecessary because the model never ran.

**Pros:**

- Deterministic. Zero LLM drift. Zero model tokens spent per spawn.
- Zero latency — no model turn required.
- No `/clear` problem at all.

**Cons:**

- Loses the `/start-session` skill's LLM-driven role classification and name
  derivation. The hook has to either (i) hard-code `--agent claude-coder` for
  every spawn, (ii) call a cheap secondary claude to classify (adds cost +
  latency), or (iii) reimplement the classifier as keyword rules in bash.
- Loses the auto-appended PR-creation and self-monitor suffixes (also
  reimplementable in bash, but forks logic between the skill and the hook).
- Escape hatch requires a stdin protocol (e.g. `!keep` prefix → hook exits 0
  without blocking).

### (c) A thurbox-level "landing input" pane

A pinned pane in the TUI (probably a `text` node with an `input` overlay)
that is always visible and, on submit, invokes `thurbox-cli session create`
directly without occupying a session slot at all. This is what the ChatGPT
metaphor most literally maps to.

**Pros:**

- The correct long-term shape. No slot spent. No `/clear` semantics. Fits
  cleanly with the v2 "interface is data" pane model.
- The pane can render lightweight spawn history (last N spawns as a scrolling
  list) without any of that living in a claude transcript.

**Cons:**

- Real feature work. Needs UI design (pane layout, focus handling, keyboard
  routing), state (spawn history), and error handling. Weeks, not days.
- Duplicates `/start-session` classification unless it shells out to a
  claude to classify, which either burns tokens per submit or falls back to
  keyword rules.

## Recommendation

**Ship (a) now. Design (c) after (a) proves the pattern out. Revisit (b) only
if drift under (a) turns out to be a real problem.**

Rationale:

- (a) reuses every primitive already in place. Estimated code: ~40 lines
  across one new symlink, one new agents.toml entry, one JSON, one small
  CLAUDE.md fragment, and one Stop-hook script.
- (a) is *reversible*: if the landing session drifts or the pattern doesn't
  earn its keep, delete the agent + symlink and the slot goes back to being
  a regular operator.
- (b) trades away the classifier and suffix logic that already exists in the
  skill. That logic is exactly the value the landing box adds vs. a raw
  `thurbox-cli` command.
- (c) is where this ends up. But building it first would gate learning
  behind a UI project that hasn't been justified yet.

## How does `/clear` get triggered

This is the delicate part of (a). Claude Code's `/clear` is a client-side
slash command; the model cannot invoke it from inside a turn (typing
`/clear` in an assistant message is just text, not a command). Options:

1. **`Stop` hook that pipes `/clear` into the pane.** Claude Code fires
   `Stop` after the model's final message of a turn. A hook script can:

   ```bash
   #!/usr/bin/env bash
   # .claude/hooks/landing-clear.sh — fires from Stop hook
   session_name="landing"  # tmux window suffix; wrapper exports this
   sleep 0.1  # let the assistant's final render finish
   tmux -L thurbox send-keys -t "thurbox:tb-${session_name}" "/clear" Enter
   ```

   Wired via `.claude/settings.json`:

   ```json
   { "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/landing-clear.sh" } ] } ] } }
   ```

   This is the layered-defense counterpart to the CLAUDE.md rule. Even if the
   model drifts and does something other than pure spawn, its context is
   wiped anyway.

   **Escape hatch integration:** the hook reads the transcript file (path
   passed in the hook input JSON) and skips the clear if the last user
   message starts with `!keep`. That keeps `!keep` conversations alive across
   turns.

2. **`thurbox-cli session restart`.** Kills the tmux window and respawns.
   Cleaner in principle, but the current restart uses `--resume`, which
   preserves history — not what we want. Would require either a
   `--fresh` flag on `session restart` (small thurbox change) or the model
   invoking `session delete <self>` + a lifecycle rule that respawns the
   deleted session, which is fragile.

3. **Do nothing; rely on `/compact`.** Not viable — compaction still keeps a
   summary in context and eventually drifts.

**Chosen:** #1 (`Stop` hook + `send-keys /clear`). Simplest, uses only
existing surfaces.

## Failure modes

| Scenario | Behaviour under (a) |
|----------|--------------------|
| `/start-session` errors (repo missing, tmux full, worktree name collision) | Skill surfaces the error in the assistant message. Stop hook fires and clears. User re-sends corrected prompt. Trade-off: the error message is lost — acceptable because the CLI error is short and the retry is cheap. Could be mitigated by having the Stop hook append the last assistant message to `~/.local/share/thurbox/landing.log` before clearing. |
| Prompt is a question, not a task ("what's the current PR queue?") | User prefixes with `!keep`. CLAUDE.md rule short-circuits: normal chat, no spawn, no clear. |
| User wants to accumulate context ("investigate this Sentry with me first, then spawn") | Same escape hatch: `!keep`. When ready, remove the prefix and the next message becomes a spawn. |
| Model drifts and starts doing the work locally | Stop hook still fires — transcript is wiped. Worst case is one wasted turn of model output. `/start-session` skill's own "ALWAYS SPAWN" warning reduces the odds. |
| Two messages arrive back-to-back (user paste, or hook firing during typing) | Each `Stop` fires its own clear. Race: user is mid-typing the second message while the first turn's clear runs. In practice: `/clear` runs; the second message's characters that were already in the input buffer are lost between `/clear` and the next `Enter`. Mitigation: don't run `/clear` if the input buffer is non-empty (the hook could `tmux display-message -p '#{pane_current_command}'` or inspect the pane, but this is fiddly). Simpler mitigation: document the pattern as "wait for the spawn UUID to print before typing the next message." Acceptable for MVP. |
| Spawn is slow (tmux backlog, worktree checkout) | The synchronous nature of `session create` makes the assistant message appear only after spawn completes. User sees the delay directly. No hidden queueing. |

## Repo defaulting

Already handled by the `/start-session` skill (step 3 of its SKILL.md):

- If `--repo` is given → use it as-is.
- Otherwise → `git rev-parse --path-format=absolute --git-common-dir`, strip
  trailing `/.git` → main repo path.

The landing session's cwd is `/Users/tch/code/spotpay/backend` (the main
backend repo, not a worktree). So the default resolves to backend without any
new alias config. If the prompt names a different repo ("in infra, add …"),
the skill's classifier is prompt-aware but repo detection is not — user must
explicitly pass `--repo` in that case. The CLAUDE.md fragment should say so:

> If the message names a repo other than backend ("in infra …", "in
> render-deploy-action …"), forward the message to `/start-session` with an
> explicit `--repo <path>`.

## Concurrency

Two spawn requests back-to-back:

- **Same tmux server, different windows:** `thurbox-cli session create` is
  serialised at the SQLite level (single writer). Second call waits ≤100ms.
- **Same landing session, two turns:** the second user message queues in
  claude's input while the first turn is running. Once the first turn ends
  (Stop hook fires clear), claude sees the second message on a clean
  transcript. This is actually the desired behaviour.
- **Clear firing while user is typing message 2:** see failure-modes table.
  Documented, not automated, for MVP.

## Migration

**Add as a 4th slot**, not a replacement. The three operator-shaped slots
each represent accumulated state that would be destroyed by conversion. The
landing box is additive: it takes over the "spawn new work" behaviour that
was informally happening in operator #1, freeing that operator to
accumulate real work.

After 1-2 weeks of use:

- If the landing box earns its keep, consider whether operator #1 can be
  merged back down to 3 slots (landing + 2 accumulating operators).
- If not, delete the agent + role file and revert to 3.

## Concrete change list (for the followup implementation PR)

Nothing in this PR — this PR is just the design doc. The implementation PR
would touch:

1. **Thurbox user config** (`~/.config/thurbox/agents.toml`, not in-repo):
   add `[[agents]] name = "claude-landing"` entry pointing to
   `thurbox-role-landing`.
2. **Shared wrapper** (`~/.local/bin/`): add symlink
   `thurbox-role-landing → thurbox-role`.
3. **Spotpay backend** (`/Users/tch/code/spotpay/backend`, separate PR
   against that repo):
   - `.claude/roles/landing.mcp.json` — empty `{"mcpServers": {}}`.
   - `.claude/roles/landing.md` — the fire-and-forget CLAUDE.md fragment.
     Loaded via `--append-system-prompt` in the wrapper when
     `role == "landing"`, or committed and loaded via the existing settings
     mechanism.
   - `.claude/hooks/landing-clear.sh` — the Stop-hook script.
   - `.claude/settings.json` — register the Stop hook, gated on
     `$CLAUDE_PROJECT_DIR`-relative role detection so it fires only in
     landing sessions (mechanism TBD in implementation PR — likely a
     wrapper-exported env var like `THURBOX_ROLE=landing`).
4. **Thurbox core** (this repo): none required for MVP. Optional future
   work: a `session create --pinned` flag so the landing box is
   auto-recreated on startup rather than manually spawned.

## Open questions for reviewers

1. **CLAUDE.md fragment loading.** Best mechanism to inject role-specific
   system-prompt text? Options: `--append-system-prompt "$(cat …)"` in the
   wrapper, or `.claude/CLAUDE.md` with role-conditional sections. Prefer
   the wrapper option because it keeps role behaviour off the default
   session.
2. **`!keep` prefix vs. an explicit slash command.** `!keep` is simple but
   collides with the existing `!` bash-passthrough convention in some
   terminals. Alternative: `.chat` prefix, or a `/landing keep` skill.
3. **Should the Stop hook append the last assistant message to a log
   before clearing?** Trades ~1KB/turn of disk for the ability to recover
   error output after a bad spawn. Recommend: yes, with weekly rotation.
4. **Startup UX.** Should thurbox auto-create the landing session on first
   run if none exists? Or leave it manual? A pinned-session concept
   (`session create --pinned`) would give a clean answer but is real
   thurbox core work.
