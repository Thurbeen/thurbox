-- The arrangement: the `split-shell` preset.
--
-- `classic` with one change: the selected session's companion shell has a pane
-- of its own, below the agent, so the two are on screen at once instead of
-- taking turns as tabs of one pane.
--
--   ┌─────────────────────────────────────────┐ header  (kernel)
--   ├──────────┬──────────────────────────────┤
--   │ sessions │           center             │
--   │  (25%)   │     the agent's terminal     │
--   │          ├──────────────────────────────┤
--   │          │            shell             │
--   │          │   the same session's shell   │
--   ├──────────┴──────────────────────────────┤
--   │ status                                  │ only when there is a message
--   ├─────────────────────────────────────────┤
--   │ footer                                  │ (kernel)
--   └─────────────────────────────────────────┘
--
-- `thurbox-cli layout set split-shell` (or `layout` in settings) wrote this
-- file. Edit it freely: thurbox stops updating a layout.lua you have changed,
-- and a later switch backs your copy up before replacing it.
--
-- It only ARRANGES, and it runs before any plugin renders — which is what lets
-- the kernel tell each plugin the rect it draws into. `header`, `status` and
-- `footer` are BANDS the kernel fills; this file decides only whether and where
-- they appear.
--
-- The shell pane is `plugins/25_shell.lua`. While it is on screen the agent pane
-- drops its own Shell tab, because one terminal cannot be drawn at two sizes;
-- wherever this file leaves the shell out — a narrow or short screen — the tab
-- comes back, so the shell is never unreachable.

local panels = require("lib.panels")

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

-- The share of the centre column the shell takes, and the least it is worth
-- showing: a shell a few rows tall shows a prompt and nothing it printed.
local SHELL_PCT = 35
local SHELL_MIN_ROWS = 8
-- The centre column must be at least this tall before it is split at all;
-- below it the agent keeps every row and the shell is its tab again.
local SPLIT_MIN_ROWS = 20

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

--- The agent pane, with the shell below it when there is room for both.
local function centre_column(ctx, rows)
  if rows < SPLIT_MIN_ROWS or not filled(ctx, "shell") then
    return { slot = "center" }
  end
  local shell_rows = math.max(SHELL_MIN_ROWS, math.floor(rows * SHELL_PCT / 100))
  return {
    axis = "vertical",
    children = {
      { slot = "center" },
      { slot = "shell", len = shell_rows },
    },
  }
end

return function(ctx)
  local height = ctx.height or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  local search = panels.shown("search") and filled(ctx, "search")
  local search_rows = search and math.min(SEARCH_ROWS, math.max(3, height - 6)) or 0
  local band_rows = (status_rows() > 0 and 1 or 0) + (height >= FOOTER_MIN_ROWS and 1 or 0)
  local header_rows = height >= HEADER_MIN_ROWS and 1 or 0
  local content_rows = height - header_rows - search_rows - band_rows

  if (ctx.width or 0) < two_panel_min_cols() then
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    -- F9 hides the list, and turning it off in the Interface tab stops the
    -- column being reserved at all.
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 25, min = 20 }
    end
    columns[#columns + 1] = centre_column(ctx, content_rows)
    children[#children + 1] = { axis = "horizontal", children = columns }
  end

  -- A strip rather than a float: it highlights matches inside the panes it is
  -- searching, which a modal over them would hide.
  if search then
    children[#children + 1] = { slot = "search", len = search_rows }
  end

  if status_rows() > 0 then
    children[#children + 1] = { slot = "status", len = 1 }
  end
  if height >= FOOTER_MIN_ROWS then
    children[#children + 1] = { slot = "footer", len = 1 }
  end

  return { children = children }
end
