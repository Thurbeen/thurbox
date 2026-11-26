local panels = require("lib.panels")

local TWO_PANEL_MIN_COLS = 80

local RIGHT_STACK_MIN_COLS = 150

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

return function(ctx)
  local width = ctx.width or 0
  local height = ctx.height or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  if width < TWO_PANEL_MIN_COLS then
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 25, min = 20 }
    end
    columns[#columns + 1] = { slot = "center" }

    local stack = {}
    if filled(ctx, "tasks") then
      stack[#stack + 1] = { slot = "tasks", pct = 55, min = 6 }
    end
    if filled(ctx, "top") then
      stack[#stack + 1] = { slot = "top", pct = 45, min = 8 }
    end
    if #stack > 0 and width >= RIGHT_STACK_MIN_COLS then
      columns[#columns + 1] = {
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
