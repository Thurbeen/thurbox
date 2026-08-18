-- A tasks pane, as a plugin.
--
-- NOT bundled. Install it into your interface with:
--
--     thurbox-cli plugin install tasks
--
-- v1 had a tasks column on `F5`/`Ctrl+W` and v2 removed it with the rest of
-- `src/ui`. The records never went anywhere — `thurbox.tasks` is in the snapshot
-- and the kernel takes `task` commands — so the pane is a file you can add back,
-- which is the whole claim the interface rests on. Nothing here is privileged:
-- reads come from the snapshot, writes go out as commands, and it declares no
-- capabilities, so it needs no trust.
--
-- What it shows that the other examples do not:
--
--   * the `input` node kind, which is the one of the four that nothing bundled
--     draws — the other three are text, box and surface
--   * an armed destructive key, so `x` cannot delete on a mistouch
--   * a pane in a slot of its OWN, placed by `docs/examples/layout.lua` — which is
--     the half people forget: adding a pane is two edits, the plugin and the slot

local theme = require("lib.theme")
local widgets = require("lib.widgets")
local textinput = require("lib.textinput")

local ROWS_MAX = 14

--- The three statuses, in the order `space` cycles them.
local ORDER = { "todo", "in_progress", "done" }

local GLYPH = { todo = "☐", in_progress = "◐", done = "☑" }

--- Colour per status, from theme ROLES rather than colours, so this pane looks
--- right under all thirty-six palettes without knowing any of them.
local function status_style(status)
  if status == "done" then
    return { fg = theme.muted }
  elseif status == "in_progress" then
    return { fg = theme.warn }
  end
  return { fg = theme.text }
end

local function tasks()
  return (thurbox and thurbox.tasks) or {}
end

--- The next status in the cycle, so one key moves a task forward.
local function next_status(current)
  for index, name in ipairs(ORDER) do
    if name == current then
      return ORDER[(index % #ORDER) + 1]
    end
  end
  return ORDER[1]
end

--- Per-pane state. `store` survives a frame; `state` would persist across
--- restarts, which a cursor position has no business doing.
local function ui()
  store.tasks_ui = store.tasks_ui or { cursor = 1, adding = nil, armed = nil }
  return store.tasks_ui
end

local function selected()
  local list = tasks()
  local at = widgets.clamp(ui().cursor, #list)
  return list[at], at
end

--- One row: glyph, title, and the source when it did not come from here.
local function row_of(task, is_cursor)
  local style = status_style(task.status)
  if is_cursor then
    style = { fg = theme.accent, bold = true }
  end
  local spans = {
    { text = "  " .. (GLYPH[task.status] or "☐") .. " ", style = style },
    { text = task.title or "(untitled)", style = style },
  }
  -- A task synced from a tracker is not yours to rename, and saying where it
  -- came from is the difference between that being obvious and being a surprise.
  if task.source and task.source ~= "local" then
    spans[#spans + 1] = { text = "  " .. task.source, style = { fg = theme.muted } }
  end
  return spans
end

local function counts()
  local open, done = 0, 0
  for _, task in ipairs(tasks()) do
    if task.status == "done" then
      done = done + 1
    else
      open = open + 1
    end
  end
  return open, done
end

return {
  name = "tasks",
  -- Its own slot, placed by `docs/examples/layout.lua` — copy that too, or this
  -- pane loads and never draws (`plugin list` says `no slot`, which is the fastest
  -- way to spot it). To use it with the SHIPPED arrangement instead, set
  -- `slot = "center"` and `slot_mode = "switch"`: it then shares the centre with
  -- the agent and F5 brings it forward.
  slot = "tasks",
  order = 80,
  focusable = true,

  keys = {
    { key = "f5", action = "tasks.open", desc = "tasks", scope = "global", group = "UI" },
    { key = "j", action = "tasks.down", desc = "next task", group = "Tasks" },
    { key = "k", action = "tasks.up", desc = "previous task", group = "Tasks" },
    { key = "space", action = "tasks.cycle", desc = "todo → doing → done", group = "Tasks" },
    { key = "n", action = "tasks.new", desc = "new task", group = "Tasks" },
    { key = "x", action = "tasks.remove", desc = "delete (twice)", group = "Tasks" },
    { key = "enter", action = "tasks.dispatch", desc = "hand to the agent", group = "Tasks" },
  },

  render = function(ctx)
    local list = tasks()
    local state = ui()
    state.cursor = widgets.clamp(state.cursor, #list)

    local children = {}
    local open, done = counts()

    if #list == 0 then
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = theme.dim("  nothing on the list — n to add one"),
      }
    else
      local rows = {}
      for index, task in ipairs(list) do
        rows[#rows + 1] = {
          spans = row_of(task, index == state.cursor and ctx.focused),
          id = tostring(task.id),
        }
      end
      local height = math.max(1, math.min(ctx.height - 5, ROWS_MAX))
      local body = widgets.list({
        rows = rows,
        selected = state.cursor,
        height = height,
      })
      body.fill = 1
      children[#children + 1] = body
    end

    -- The `input` kind, drawn only while it is wanted: a field that is always
    -- there is a row spent on nothing most of the time.
    if state.adding then
      children[#children + 1] = textinput.node(state.adding, {
        label = "New task",
        focused = true,
      })
    end

    if state.armed then
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = {
          {
            { text = "  delete ", style = { fg = theme.bad } },
            { text = state.armed.title or "", style = { fg = theme.bad, bold = true } },
            { text = "?  x again to confirm", style = { fg = theme.bad } },
          },
        },
      }
    end

    children[#children + 1] = widgets.divider(ctx.width - 2)
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = {
        {
          { text = "  " .. open .. " open", style = { fg = theme.text } },
          { text = "  ·  " .. done .. " done", style = { fg = theme.muted } },
        },
      },
    }
    children[#children + 1] = widgets.hints({
      { "space", "status" },
      { "n", "new" },
      { "enter", "to agent" },
      { "x", "delete" },
    })

    return {
      type = "box",
      frame = widgets.panel("Tasks", ctx.focused),
      children = children,
    }
  end,

  on_action = function(action)
    local state = ui()
    local task = selected()

    if action == "tasks.open" then
      command("focus", { text = "tasks" })
      return true
    end
    if action == "tasks.down" or action == "tasks.up" then
      -- Any movement disarms: an armed delete that survives the cursor moving
      -- off it is a delete aimed at whatever you land on.
      state.armed = nil
      local step = action == "tasks.down" and 1 or -1
      state.cursor = widgets.clamp(state.cursor + step, #tasks())
      return true
    end
    if action == "tasks.new" then
      state.adding = textinput.new("")
      return true
    end
    if not task then
      return false
    end
    if action == "tasks.cycle" then
      state.armed = nil
      command("task", { number = task.id, status = next_status(task.status) })
      return true
    end
    if action == "tasks.dispatch" then
      state.armed = nil
      -- `dispatch` sends the task to a session; with none named the kernel picks
      -- the selected one, which is what a reader means by "hand it to the agent".
      command("dispatch", { number = task.id })
      return true
    end
    if action == "tasks.remove" then
      -- Armed rather than confirmed in a modal: a modal for every delete is what
      -- teaches people to dismiss modals without reading them.
      if state.armed and state.armed.id == task.id then
        state.armed = nil
        command("task", { number = task.id, reset = true })
      else
        state.armed = { id = task.id, title = task.title }
      end
      return true
    end
    return false
  end,

  -- Keys the pane did not declare arrive here, which is how the input field gets
  -- ordinary letters without every one of them being an action.
  on_key = function(key)
    local state = ui()
    if not state.adding then
      return false
    end
    if key.name == "enter" then
      local title = (state.adding.value or ""):match("^%s*(.-)%s*$")
      if title ~= "" then
        command("task", { text = title })
      end
      state.adding = nil
      return true
    end
    if key.name == "esc" then
      state.adding = nil
      return true
    end
    return textinput.key(state.adding, key)
  end,
}
