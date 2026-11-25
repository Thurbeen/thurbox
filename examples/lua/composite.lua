local theme = require("lib.theme")
local widgets = require("lib.widgets")

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

local function answer(key)
  local runs = (thurbox and thurbox.runs) or {}
  local got = runs[key]
  if got and got.state == "done" then
    return got
  end
  return nil, got
end

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

local function cell(label, value, role, width)
  local text = tostring(value)
  local gap = math.max(1, width - widgets.len(label) - widgets.len(text))
  return {
    { text = " " .. label, style = { fg = theme.muted } },
    { text = string.rep(" ", gap) .. text, style = { fg = theme[role] or theme.text } },
  }
end

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
