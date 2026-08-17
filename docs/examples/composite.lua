-- A pane over programs you already run.
--
-- Two of them at once — `git status --porcelain` and `git log --oneline` — each
-- parsed rather than echoed, drawn as a table whose rows carry their own state.
-- That combination is the point: a pane that shells out and prints the output
-- verbatim is a worse terminal, and thurbox already has a real one (`Ctrl+T`).
-- What a pane is *for* is deriving something you can read at a glance.
--
-- To use it, copy it into your interface directory (`thurbox-cli plugin dir`),
-- then trust it: settings (`Ctrl+,`) → `]` → find it → `t`. Until you do, `run`
-- is not a function here and the pane says so. That is not an error state; it is
-- the state every plugin starts in, and drawing it well is part of the job.
--
-- THE ONE RULE THAT MATTERS. Ask on every frame. `run` does nothing while the
-- answer is fresh, so asking is a map lookup, not a process — and the alternative
-- (asking once, somewhere clever) is how a pane ends up showing yesterday's
-- answer forever.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

--- The session this pane is about: whatever the list has selected.
local function selected()
  local id = store.selected
  if not id then
    return nil
  end
  for _, session in ipairs((thurbox and thurbox.sessions) or {}) do
    if session.id == id then
      return session
    end
  end
  return nil
end

--- One run's answer, or nil while it has not arrived.
local function answer(key)
  local runs = (thurbox and thurbox.runs) or {}
  local got = runs[key]
  if got and got.state == "done" then
    return got
  end
  return nil, got
end

--- `git status --porcelain` as counts by kind.
---
--- The parse is the pane's whole value: two numbers you can read in a glance,
--- out of output you would otherwise have to scan.
local function tally(text)
  local counts = { modified = 0, added = 0, deleted = 0, untracked = 0 }
  for line in (text or ""):gmatch("[^\n]+") do
    local code = line:sub(1, 2)
    if code:find("?") then
      counts.untracked = counts.untracked + 1
    elseif code:find("D") then
      counts.deleted = counts.deleted + 1
    elseif code:find("A") then
      counts.added = counts.added + 1
    elseif code:find("%S") then
      counts.modified = counts.modified + 1
    end
  end
  return counts
end

--- A table row: a label, a value, and the state that colours it.
local function cell(label, value, role, width)
  local text = tostring(value)
  local gap = math.max(1, width - widgets.len(label) - widgets.len(text))
  return {
    { text = " " .. label, style = { fg = theme.muted } },
    { text = string.rep(" ", gap) .. text, style = { fg = theme[role] or theme.text } },
  }
end

--- What to draw when the pane has not been trusted.
---
--- Deliberately not blank and not an error: it is the first thing every user of
--- this plugin sees, and a pane that looks broken before you have done anything
--- is a plugin nobody keeps.
local function untrusted(ctx)
  return {
    type = "box",
    frame = widgets.panel("Repo", ctx.focused),
    children = {
      { type = "text", len = 1, text = "" },
      {
        type = "text",
        len = 1,
        text = { { { text = "  this pane runs git for you", style = { fg = theme.text } } } },
      },
      {
        type = "text",
        len = 1,
        text = {
          {
            { text = "  trust it in settings → Interface → ", style = { fg = theme.muted } },
            { text = "t", style = { fg = theme.accent, bold = true } },
          },
        },
      },
    },
  }
end

return {
  name = "composite",
  slot = "center",
  slot_mode = "switch",
  order = 95,
  focusable = true,

  -- Declaring it is not being granted it. Without the user's trust the `run`
  -- global is absent, which is why `render` checks rather than assuming.
  capabilities = { "run" },

  keys = {
    {
      key = "f8",
      action = "composite.open",
      desc = "repo at a glance",
      scope = "global",
      group = "UI",
    },
    { key = "r", action = "composite.refresh", desc = "read it again now", group = "Repo" },
  },

  render = function(ctx)
    if not run then
      return untrusted(ctx)
    end
    local session = selected()
    if not session then
      return {
        type = "box",
        frame = widgets.panel("Repo", ctx.focused),
        children = { { type = "text", text = theme.dim("  no session selected") } },
      }
    end

    -- Asked every frame, on purpose. Both at once, so neither waits for the
    -- other — the fast one draws while the slow one is still running.
    run("status", "git status --porcelain", { session = session.id, ttl = 2 })
    run("recent", "git log --oneline -n 5", { session = session.id, ttl = 30 })

    local width = math.max(0, (ctx.width or 0) - 4)
    local lines = {}

    local status, pending = answer("status")
    if status and status.ok then
      local counts = tally(status.stdout)
      lines[#lines + 1] = { spans = cell("modified", counts.modified, "warn", width) }
      lines[#lines + 1] = { spans = cell("added", counts.added, "ok", width) }
      lines[#lines + 1] = { spans = cell("deleted", counts.deleted, "bad", width) }
      lines[#lines + 1] = { spans = cell("untracked", counts.untracked, "muted", width) }
    elseif status then
      lines[#lines + 1] =
        { spans = { { text = "  not a git repository", style = { fg = theme.muted } } } }
    else
      local what = pending and pending.state == "failed" and pending.error or "reading…"
      lines[#lines + 1] = { spans = { { text = "  " .. what, style = { fg = theme.muted } } } }
    end

    lines[#lines + 1] = { spans = { { text = "" } } }

    local recent = answer("recent")
    if recent and recent.ok then
      for line in recent.stdout:gmatch("[^\n]+") do
        lines[#lines + 1] = {
          spans = {
            { text = "  " .. widgets.truncate(line, width), style = { fg = theme.secondary } },
          },
        }
      end
    end

    local list = widgets.list({ rows = lines, height = math.max(0, (ctx.height or 0) - 3) })
    list.fill = 1

    return {
      type = "box",
      frame = widgets.panel("Repo", ctx.focused),
      children = {
        list,
        widgets.hints({ { "r", "read again" } }),
      },
    }
  end,

  on_action = function(action)
    if action == "composite.open" then
      command("focus", { text = "composite" })
      return true
    elseif action == "composite.refresh" then
      -- `refresh` is what overrides freshness; without it this key would do
      -- nothing, because the answer it wants is the one already on screen.
      local session = selected()
      if run and session then
        run("status", "git status --porcelain", { session = session.id, refresh = true })
        run("recent", "git log --oneline -n 5", { session = session.id, refresh = true })
      end
      return true
    end
    return false
  end,
}
