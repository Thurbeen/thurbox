-- A demo arrangement: sessions | agent | tasks over top.
--
-- NOT bundled. It REPLACES the shipped arrangement, so back that one up first —
-- or just delete yours afterwards and the Interface tab will restore it:
--
--     cp docs/examples/layout.lua   ~/.config/thurbox/ui/layout.lua
--     cp docs/examples/tasks.lua    ~/.config/thurbox/ui/plugins/80_tasks.lua
--     cp docs/examples/top.lua      ~/.config/thurbox/ui/plugins/85_top.lua
--
-- Then `F10`, and trust the `top` plugin (settings → Interface → `t`) so it may run
-- a program.
--
--     ┌──────────────────────────────────────────────────┐ header
--     ├──────────┬───────────────────────────┬───────────┤
--     │ sessions │          center           │   tasks   │
--     │   25%    │       (remainder)         │    55%    │
--     │          │                           ├───────────┤
--     │          │                           │    top    │
--     │          │                           │    45%    │
--     ├──────────┴───────────────────────────┴───────────┤
--     │ status                                           │ only with a message
--     ├──────────────────────────────────────────────────┤
--     │ footer                                           │
--     └──────────────────────────────────────────────────┘
--
-- The point of the file: the right column is a nested `axis = "vertical"` box
-- inside the horizontal row, which is all "stacked" means here. Nothing about the
-- two panes decided this — they name a slot and the arrangement decides where a
-- slot goes, which is why the same plugin can be a side column here and the whole
-- centre somewhere else.
--
-- It only ARRANGES. It never calls a plugin, so a mistake here cannot break one.

local panels = require("lib.panels")

local TWO_PANEL_MIN_COLS = 80

--- Columns below which the right stack is dropped rather than squeezed.
---
--- A gauge in twelve columns is not a gauge, and a task title truncated to eight
--- characters is not a task title. Better to have neither than both illegible.
local RIGHT_STACK_MIN_COLS = 150

local HEADER_MIN_ROWS = 20
local FOOTER_MIN_ROWS = 4
local SEARCH_ROWS = 12

local function status_rows()
  return (thurbox and thurbox.chrome and thurbox.chrome.status_rows) or 0
end

--- Will a loaded plugin actually paint into `slot`?
---
--- Same guard the shipped arrangement uses, and the reason it matters more here:
--- these two panes are examples, so the likeliest state of this file is one where
--- you have copied it and not (yet) both plugins. A slot no plugin can fill is a
--- rect reserved for nothing — an empty column, which reads as breakage.
---
--- Unknown answers count as filled: the arrangement runs before anything else and
--- must never fail closed.
local function filled(ctx, slot)
  local slots = ctx and ctx.slots
  if type(slots) ~= "table" then
    return true
  end
  return slots[slot] == true
end

return function(ctx)
  local width = ctx.width or 0
  local height = ctx.height or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  if width < TWO_PANEL_MIN_COLS then
    -- Too narrow for anything beside the agent, as the shipped file also decides.
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 25, min = 20 }
    end
    -- The centre is never dropped and takes whatever the columns leave.
    columns[#columns + 1] = { slot = "center" }

    -- The right stack. Built as a list first so a missing plugin costs its own
    -- row rather than the whole column: with only `tasks` copied you get tasks
    -- full height, and with neither you get no column at all.
    local stack = {}
    if filled(ctx, "tasks") then
      stack[#stack + 1] = { slot = "tasks", pct = 55, min = 6 }
    end
    if filled(ctx, "top") then
      stack[#stack + 1] = { slot = "top", pct = 45, min = 8 }
    end
    if #stack > 0 and width >= RIGHT_STACK_MIN_COLS then
      columns[#columns + 1] = {
        -- "Stacked vertically" is this line and nothing else.
        axis = "vertical",
        pct = 30,
        min = 28,
        children = stack,
      }
    end

    children[#children + 1] = { axis = "horizontal", children = columns }
  end

  if panels.shown("search") and filled(ctx, "search") then
    children[#children + 1] =
      { slot = "search", len = math.min(SEARCH_ROWS, math.max(3, height - 6)) }
  end
  if status_rows() > 0 then
    children[#children + 1] = { slot = "status", len = 1 }
  end
  if height >= FOOTER_MIN_ROWS then
    children[#children + 1] = { slot = "footer", len = 1 }
  end

  return { children = children }
end
