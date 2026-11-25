local panels = require("lib.panels")

local TWO_PANEL_MIN_COLS_DEFAULT = 80

local function two_panel_min_cols()
  local settings = thurbox and thurbox.settings
  return (settings and settings.two_panel_min_cols) or TWO_PANEL_MIN_COLS_DEFAULT
end

local HEADER_MIN_ROWS = 20
local FOOTER_MIN_ROWS = 4

local SEARCH_ROWS = 12

local function status_rows()
  return (thurbox and thurbox.chrome and thurbox.chrome.status_rows) or 0
end

local function filled(ctx, slot)
  local slots = ctx and ctx.slots
  if type(slots) ~= "table" then
    return true
  end
  return slots[slot] == true
end

local function places(children, slot)
  for _, child in ipairs(children) do
    if child.slot == slot or (child.children and places(child.children, slot)) then
      return true
    end
  end
  return false
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
    children[#children + 1] = { axis = "horizontal", children = columns }
  end

  if panels.shown("search") and filled(ctx, "search") then
    children[#children + 1] =
      { slot = "search", len = math.min(SEARCH_ROWS, math.max(3, height - 6)) }
  end

  local strips_at = #children + 1

  if status_rows() > 0 then
    children[#children + 1] = { slot = "status", len = 1 }
  end
  if height >= FOOTER_MIN_ROWS then
    children[#children + 1] = { slot = "footer", len = 1 }
  end

  for _, strip in ipairs(ctx.strips or {}) do
    if not places(children, strip.slot) then
      table.insert(children, strips_at, { slot = strip.slot, len = strip.len })
      strips_at = strips_at + 1
    end
  end

  return { children = children }
end
