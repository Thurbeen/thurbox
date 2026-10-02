-- The arrangement: the `focus` preset.
--
-- The agent alone, the whole width — VS Code's Zen mode, JetBrains'
-- Distraction-Free mode. Every column starts closed, and each one's own toggle
-- brings it back: F9 the session list, a plugin column its own key.
--
--   ┌─────────────────────────────────────────┐ header  (kernel)
--   ├─────────────────────────────────────────┤
--   │                 center                  │
--   │          the agent's terminal           │
--   │                                         │
--   ├─────────────────────────────────────────┤
--   │ status                                  │ only when there is a message
--   ├─────────────────────────────────────────┤
--   │ footer                                  │ (kernel)
--   └─────────────────────────────────────────┘
--
-- `thurbox-cli layout set focus` (or `layout` in settings) wrote this file. Edit
-- it freely: thurbox stops updating a layout.lua you have changed, and a later
-- switch backs your copy up before replacing it.
--
-- It only ARRANGES, and it runs before any plugin renders — which is what lets
-- the kernel tell each plugin the rect it draws into. `header`, `status` and
-- `footer` are BANDS the kernel fills; this file decides only whether and where
-- they appear. The shell stays a tab of the agent pane, since no shell pane is
-- placed here.

local panels = require("lib.panels")

-- The list starts closed here rather than open as everywhere else. Declared
-- rather than read differently, so F9 flips what is on screen on its first
-- press and the agent pane's chevron agrees with it.
panels.starts("sessions", false)

-- `Settings::two_panel_min_cols`: below it there is room for the agent alone.
-- The fallback keeps a kernel that published nothing on a working screen.
local TWO_PANEL_MIN_COLS_DEFAULT = 80

local function two_panel_min_cols()
  local settings = thurbox and thurbox.settings
  return (settings and settings.two_panel_min_cols) or TWO_PANEL_MIN_COLS_DEFAULT
end

-- Below 20 rows the header goes first, the footer below 4.
local HEADER_MIN_ROWS = 20
local FOOTER_MIN_ROWS = 4

local SEARCH_ROWS = 12

-- Slots this file places by name. Any other a plugin fills is a third-party
-- column (a file tree, a queue), placed on the right once its toggle opens it.
local KNOWN = { sessions = true, center = true, shell = true, search = true }

local function status_rows()
  return (thurbox and thurbox.chrome and thurbox.chrome.status_rows) or 0
end

--- Will a loaded plugin actually paint into `slot`?
---
--- The panel toggles are arrangement state, the Interface tab's disabled set is
--- delivery state, and a rect is worth reserving only when both agree. Unknown
--- answers count as filled: the arrangement must never fail closed.
local function filled(ctx, slot)
  local slots = ctx and ctx.slots
  if type(slots) ~= "table" then
    return true
  end
  return slots[slot] == true
end

--- Third-party columns someone has opened, in a stable order.
local function opened_columns(ctx)
  local names = {}
  for _, slot in ipairs(panels.others(ctx, KNOWN)) do
    if panels.shown(slot) then
      names[#names + 1] = slot
    end
  end
  return names
end

return function(ctx)
  local height = ctx.height or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  if (ctx.width or 0) < two_panel_min_cols() then
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 25, min = 20 }
    end
    columns[#columns + 1] = { slot = "center" }
    for _, slot in ipairs(opened_columns(ctx)) do
      columns[#columns + 1] = { slot = slot, pct = 25, min = 30 }
    end
    children[#children + 1] = { axis = "horizontal", children = columns }
  end

  -- A strip rather than a float: it highlights matches inside the panes it is
  -- searching, which a modal over them would hide.
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
