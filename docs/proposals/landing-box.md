# Proposal: Landing-box operator for one-shot session spawn

**Status:** draft — implementation not yet greenlit
**Author:** <thomas@spotpay.us>
**Related:** `~/.config/thurbox/ui/plugins/70_new_session.lua` (reference:
existing session-creation pane), `~/.config/thurbox/ui/AGENTS.md`,
`~/.config/thurbox/ui/README.md` (five rules), `~/dev/skills/thurbox/.claude/skills/start-session/SKILL.md`

## Problem

Three "operator-shaped" thurbox session slots exist today (operator, debugger,
dispatcher). In practice one gets spent as a scratchpad for **new work
initiation** — the user types a short prompt ("investigate sentry 733…",
"add a bulk-transfer endpoint"), the operator classifies it and spawns a
dedicated worktree session via `/start-session`, and then the operator's
context is polluted with routing metadata that has no long-term value. A
week in, that operator's transcript is mostly spawn receipts and the user
has to `/clear` it anyway.

The desired shape is **ChatGPT's landing input**: an always-available textbox
whose sole purpose is "turn this sentence into a fresh session." Zero
accumulated state. No slot spent.

## The customization surface I initially missed

Thurbox v2's interface **is** the Lua directory at
`~/.config/thurbox/ui/plugins/`. Every pane on screen is a file there,
live-reloaded on save. The kernel exposes:

- Four node kinds: `text`, `box`, `input`, `surface`.
- Snapshot reads via a global `thurbox` table (`thurbox.sessions`, `.agents`,
  `.repos`, `.settings`, …).
- Kernel-write via a global `command(name, args)` — accepted synchronously,
  applied on a worker. Examples already in the tree: `command("create", …)`,
  `command("fork", …)`, `command("delete", …)`, `command("focus", …)`,
  `command("bookmark", …)`.
- A `lib/` of shared widgets (`textinput`, `theme`, `widgets`) that every
  pane imports.
- `thurbox-cli plugin new <name>` writes a starter that already loads;
  `thurbox-cli plugin check` fails on both load errors and the silent
  "loaded-but-nothing-drawn" case, printing the `layout.lua` line to add.

The 1,487-line `70_new_session.lua` is proof of the ceiling: a full 6-step
wizard (`host → repo → base → name → branch → agent → create`) in one file,
ending in exactly:

```lua
command("create", {
  text   = flow.name.value,
  repo   = flow.primary,
  branch = flow.base and flow.branch.value or nil,
  base   = flow.base,
  agent  = agent,
  host   = (flow.host ~= "" and flow.host) or nil,
  extras = flow.extras or {},
})
```

A landing-box pane is **that same call**, minus the six wizard steps —
defaults for repo/base/agent, name derived from the prompt, and one extra
step: piping the prompt into the newly-created session.

## Design options (revised)

### (a) Native Lua pane — **recommended**

One file, `~/.config/thurbox/ui/plugins/05_landing.lua` (early `order` so it
sits at the top). Shape:

```lua
local textinput = require("lib.textinput")
local theme     = require("lib.theme")
local widgets   = require("lib.widgets")

local NAME = "landing"
local DEFAULT_REPO  = "/Users/tch/code/spotpay/backend"
local DEFAULT_BASE  = "main"

-- Keyword classifier. Same table as the /start-session skill.
local function classify(prompt)
  local p = prompt:lower()
  if p:match("sentry") or p:match("crash") or p:match("ci fail")
     or p:match("bug") or p:match("error") then
    return "debugger"
  end
  if p:match("admin ") or p:match("lookup") or p:match("investigate user") then
    return "operator"
  end
  return "coder"     -- default: proactive build work
end

local function slug(prompt)
  local s = prompt:lower():gsub("[^%w]+", "-"):gsub("^-+", ""):gsub("-+$", "")
  return s:sub(1, 40)
end

local function submit(prompt)
  local role = classify(prompt)
  local name = role .. "-" .. slug(prompt)
  command("create", {
    text   = name,
    repo   = DEFAULT_REPO,
    base   = DEFAULT_BASE,
    branch = name,
    agent  = "claude-" .. role,
    extras = { pending_prompt = prompt },   -- see § Prompt injection
  })
end

return {
  name = NAME,
  slot = "top",           -- placed via layout.lua; pinned in the arrangement
  order = 5,
  focusable = true,
  keys = {
    { key = "f3", action = "landing.focus", desc = "landing box",
      scope = "global" },
  },
  render = function(ctx)
    local input = state.input or textinput.new("")
    state.input = input
    return {
      type = "box",
      frame = widgets.panel("Landing", ctx.focused),
      children = { widgets.textline({ input = input, prompt = "» " }) },
    }
  end,
  on_key = function(key)
    if key.name == "enter" then
      local text = (state.input.value or ""):match("^%s*(.-)%s*$")
      if text ~= "" then
        submit(text)
        textinput.clear(state.input)
      end
      return true
    end
    return textinput.key(state.input, key)
  end,
}
```

Plus one line in `layout.lua`:

```lua
if filled(ctx, "top") then
  columns[#columns + 1] = { slot = "top", pct = 100, min = 3, height = 3 }
end
```

**Pros:**

- No session slot spent. The pane is *screen* real-estate, not a claude session.
- No `/clear` semantics required. There is no landing session; each spawn is
  a fresh worktree session and that's the end of it.
- Kernel handles the create atomically; the SQLite writer serialises
  concurrent submits.
- Live-reload: iterate on the pane by saving. `thurbox-cli plugin check`
  gates each save.
- The 1,487-line `70_new_session.lua` already proves everything harder than
  this works. AGENTS.md exists specifically to let an agent (this one, in a
  future implementation session) safely edit the directory.
- Reversible: `space` on the pane's row in `Ctrl+,` → `]` turns it off; the
  file stays on disk, untouched.

**Cons:**

- **Prompt injection to the just-created session is the load-bearing unknown.**
  `command("create", …)` returns immediately; the pane doesn't get the UUID
  synchronously. Three fallbacks, in order of preference — see the
  "Prompt injection" section below.
- Keyword-based classifier is dumber than the `/start-session` skill's
  LLM-driven one. Acceptable because most spawns are unambiguously "coder"
  or "debugger"; the escape hatch (§ Escape hatches) covers the rest.

### (b) New `landing` role in `agents.toml`

A dedicated persistent claude session whose CLAUDE.md hard-codes
"every message becomes `/start-session <message>`, then a `Stop` hook fires
`/clear` via `tmux send-keys`."

**Pros:**

- Uses the existing role-layering (`.claude/roles/landing.mcp.json` +
  `thurbox-role-landing` wrapper).
- Inherits the `/start-session` skill's LLM classifier + suffix injection.

**Cons:**

- Costs a session slot (a 4th slot, or displaces an accumulating operator).
- Model can drift; `/clear` via `Stop` hook is a layered defence but adds
  moving parts.
- Every spawn costs one model turn of tokens on the landing session, on top
  of whatever the spawned session burns.
- Every spawn incurs one full model round-trip of latency before the actual
  work session even starts.

### (c) `UserPromptSubmit` hook that short-circuits the model

A hook on a dedicated claude session that intercepts each prompt, calls
`thurbox-cli session create` + `session send` directly from bash, then emits
a `UserPromptSubmit` `hookSpecificOutput` with `permissionDecision: "deny"`
so the model never runs.

**Pros:**

- Deterministic. Zero model tokens per spawn. Zero model latency.

**Cons:**

- Still costs a session slot.
- Reimplements classification + name derivation + suffix injection in bash,
  forking logic from the `/start-session` skill.

## Recommendation

**Ship (a). Fall back to (b) only if the prompt-injection question below
turns out to have no clean answer.**

Rationale:

- (a) matches the ChatGPT landing metaphor most literally: it's a *textbox*,
  not a session.
- (a) is the shape the customization surface was designed for. AGENTS.md
  goes so far as to advertise "point a coding agent at this directory and
  ask for a pane." The tooling (`plugin new`, `plugin check`, live reload)
  is built around this workflow.
- (a) frees all 3 existing operator slots for accumulated work.
- (b) and (c) both consume a session slot for what should be UI. Reasonable
  fallbacks, but only if (a) hits a wall.

## Prompt injection — the load-bearing detail

`command("create", …)` fires and forgets; the UUID appears in
`thurbox.sessions` on a later frame. Three ways to get the prompt into the
new session, ranked by preference:

### Option 1 — extend `command("create")` with an `initial_prompt` field

Small thurbox core change: after the tmux window is live and the agent
launched, the kernel writes the initial prompt into the pane via the same
`tmux paste-buffer -p ; send-keys Enter` sequence the `/start-session` skill
uses today. The Lua pane just passes `initial_prompt = text` to `create`.

Cleanest. Composable — every other pane that spawns sessions gains this too.
Estimated: <100 lines of Rust in the session-create path.

### Option 2 — pane polls `thurbox.sessions`, then `command("send", …)`

If `command("send", { session = uuid, text = prompt })` is wired at the
kernel level (the CLI `thurbox-cli session send` exists — the question is
whether the same command is registered for the Lua write side), the pane
stores the pending prompt keyed by session name, watches
`thurbox.sessions` for the row to appear, then fires `send` and clears the
pending entry. All in Lua, no core changes.

I could not confirm from static inspection whether `send` is a valid
`command(...)` name from Lua. `thurbox-cli plugin check` on a stub pane
that calls it would answer this in <30s during implementation.

### Option 3 — `run` capability shelling out to `thurbox-cli session send`

Least preferred: the pane declares `capabilities = { "run" }`; user grants
it via `Ctrl+,` → `]` → `t`; on the frame the new session appears, the pane
calls `run("send", { "thurbox-cli", "session", "send", uuid, prompt })`.

Works today with no core changes. Downside: extra trust prompt, extra
process per spawn.

**Decision path for implementation:** try Option 2 first (`plugin check` on a
one-liner reveals it in seconds). Fall back to Option 3 if `send` isn't
exposed. Escalate to Option 1 only if we want the cleaner composable shape
for reuse by other panes.

## Failure modes

| Scenario | Behaviour under (a) |
|----------|--------------------|
| `command("create", …)` refused (repo missing, name collision, worktree in-flight) | Kernel emits a failed command; the message band reports it (that's the standard mechanism the rest of the interface uses). The pane's input is not cleared until submit succeeds — user can correct and retry. |
| Prompt is a question, not a task ("what's the current PR queue?") | Not applicable — there is no landing session to chat with. User asks in one of the operator sessions instead. |
| User wants to accumulate context before spawning | Not applicable. Same as above: use an operator session. |
| Pane loads but nothing draws | Exactly what `thurbox-cli plugin check` fails on, with the missing `layout.lua` line printed. Caught pre-commit if we wire it into the spotpay-backend hooks, otherwise on `F10` reload. |
| Prompt injection fires before session boot is complete | The `/start-session` skill has proven the "paste on top of a booting claude" pattern works — claude buffers stdin during startup. If Option 1 is chosen, the kernel does the paste itself and can wait on backend-window-ready. |
| Concurrent submits | SQLite serialises the create writes. Second submit waits ≤100ms. Two worktrees materialise back-to-back. |
| Wrong classification | User re-types with an explicit prefix (`--role debugger …`) — the pane's `classify()` can recognise this leading token and skip auto-detection. Escape hatch documented below. |

## Escape hatches

- **Prefix `--role <role>`**: `classify()` recognises the leading token,
  strips it, and uses the given role.
- **Prefix `--repo <path>`**: same treatment; overrides `DEFAULT_REPO`.
- **Empty submit**: no-op (guarded in `on_key`).
- **`Esc` / focus-away**: input keeps its text across frames (state is
  plugin-scoped and survives reloads). Nothing spawns until `Enter`.

## Repo defaulting

`DEFAULT_REPO = "/Users/tch/code/spotpay/backend"` in the pane. Prompts that
name another repo either use the `--repo` escape hatch or a small extension
to `classify()` (e.g. `"in infra …"` → `repo = infra path`). Kept simple for
MVP: hard-coded backend, `--repo` opt-out.

## Concurrency

- **Same submit fires twice (double-Enter):** input clears on the first
  successful `command("create", …)` — the second Enter sees an empty input
  and no-ops.
- **Two submits in rapid succession:** each becomes its own `create` command.
  SQLite writer serialises. Both sessions materialise; both prompts inject
  independently (all three options are per-session, no cross-talk).
- **Submit while a create is in-flight:** the pane could show a spinner
  reading `thurbox.commands` (the pattern `70_new_session.lua` uses for
  `bookmark_pending`), but MVP can skip and just let the second `create`
  queue.

## Migration

- **Add the pane immediately.** Zero displacement — it's a screen slot, not
  a session slot. All 3 operator sessions remain untouched.
- **Observe for 1-2 weeks.** Do operators shed their "spawn scratchpad"
  behaviour? Does the pane get used?
- **If successful, no further migration needed.** If unused, `space` in the
  interface tab turns it off; the file stays on disk for later revival.

## Concrete change list (for the followup implementation PR)

Nothing in this PR — this PR is just the design doc. The implementation PR
would touch **only the user's UI directory** (`~/.config/thurbox/ui/`), not
this repo:

1. `~/.config/thurbox/ui/plugins/05_landing.lua` — the pane above.
2. `~/.config/thurbox/ui/layout.lua` — one-line addition for the `top`
   slot (or reuse an existing slot if the arrangement already has one).
3. Optional: a `plugins.toml` entry if we want to publish it as an
   installable plugin later; MVP just lives in the user copy.

Zero changes to the thurbox binary, zero changes to spotpay/backend, zero
changes to `~/.config/thurbox/agents.toml`, unless Option 1 (kernel
`initial_prompt`) is chosen — in which case the followup PR against
`Thurbeen/thurbox` is <100 lines in the session-create path.

## Open questions for reviewers

1. **Prompt injection option 1 vs 2 vs 3.** Which of the three routes above
   do we want? Cheapest to answer with a five-minute experiment during
   implementation. Recommend: try Option 2 first, promote to Option 1 if we
   see other panes benefit.
2. **Slot choice.** `top` (full-width, 3 rows) vs. squeezing into an existing
   pane. `top` is honest and unmissable; a bottom minibuffer is more subtle.
3. **Classifier depth.** Keep the ~10-line keyword classifier in Lua, or
   delegate classification to a spawned session by always spawning
   `claude-coder` with a prompt of `"/start-session " .. text` (turning
   every landing spawn into an intermediate "invoke the skill" session).
   The latter reuses the skill's LLM classifier but costs one extra session
   birth per submit. Recommend: keyword classifier first, escalate if
   misclassification actually bites.
4. **Suffix injection.** The `/start-session` skill appends PR-creation and
   self-monitor blocks to every prompt. The pane should do the same before
   `command("send", …)`. Trivial to hoist the suffixes into a Lua string
   constant.
