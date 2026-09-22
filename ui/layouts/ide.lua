-- The arrangement: the `ide` preset.
--
-- The shape VS Code and JetBrains open with: the session list where their
-- explorer is, the agent where the editor is, the shell as the panel along the
-- bottom of it, and a right-hand column (VS Code's secondary side bar) for
-- whatever other panes are installed — a file tree, a queue, a pipelines view.
--
--   ┌──────────────────────────────────────────────────────┐ header  (kernel)
--   ├──────────┬──────────────────────────────┬────────────┤
--   │ sessions │           center             │   right    │
--   │  (20%)   │     the agent's terminal     │   (22%)    │
--   │          ├──────────────────────────────┤  every     │
--   │          │            shell             │  other     │
--   │          │   the same session's shell   │  pane      │
--   ├──────────┴──────────────────────────────┴────────────┤
--   │ status                                               │ only with a message
--   ├──────────────────────────────────────────────────────┤
--   │ footer                                               │ (kernel)
--   └──────────────────────────────────────────────────────┘
--
-- The right column exists only while a plugin fills it, so with thurbox's own
-- panes alone the agent runs to the right edge. It is the first thing to go
-- as the screen narrows (below `three_panel_min_cols`), then the list and the
-- shell (below `two_panel_min_cols`), and the shell also leaves a short screen.
--
-- `thurbox-cli layout set ide` (or `layout` in settings) wrote this file. Edit
-- it freely: thurbox stops updating a layout.lua you have changed, and a later
-- switch backs your copy up before replacing it.
--
-- It only ARRANGES, and it runs before any plugin renders — which is what lets
-- the kernel tell each plugin the rect it draws into. `header`, `status` and
-- `footer` are BANDS the kernel fills; this file decides only whether and where
-- they appear.
--
-- The shell pane is `plugins/25_shell.lua`. While it is on screen the agent pane
-- drops its own Shell tab, because one terminal cannot be drawn at two sizes;
-- wherever this file leaves the shell out the tab comes back.

local panels = require("lib.panels")

-- `Settings::two_panel_min_cols` and `three_panel_min_cols`, with their
-- defaults, so a kernel that published nothing still gets a working screen.
local TWO_PANEL_MIN_COLS_DEFAULT = 80
local THREE_PANEL_MIN_COLS_DEFAULT = 120

local function setting(name, default)
  local settings = thurbox and thurbox.settings
  return (settings and settings[name]) or default
end

-- Below 20 rows the header goes first, the footer below 4.
local HEADER_MIN_ROWS = 20
local FOOTER_MIN_ROWS = 4

local SEARCH_ROWS = 12

-- The share of the centre column the shell takes, and the least it is worth
-- showing: a shell a few rows tall shows a prompt and nothing it printed.
local SHELL_PCT = 30
local SHELL_MIN_ROWS = 8
-- The centre column must be at least this tall before it is split at all;
-- below it the agent keeps every row and the shell is its tab again.
local SPLIT_MIN_ROWS = 20

-- Slots this file places by name. Any other a plugin fills goes to the right.
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

--- The right-hand panes on screen, in a stable order.
---
--- Each starts open here — the column is the point of this preset — and its
--- own toggle closes it. Declaring the start state, rather than reading "unset"
--- as open, is what makes that toggle's first press close it.
local function right_panes(ctx)
  local names = {}
  if type(ctx.slots) == "table" then
    for slot, occupied in pairs(ctx.slots) do
      if occupied == true and not KNOWN[slot] then
        panels.starts(slot, true)
        if panels.shown(slot) then
          names[#names + 1] = slot
        end
      end
    end
  end
  table.sort(names)
  return names
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
  local width = ctx.width or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  local search = panels.shown("search") and filled(ctx, "search")
  local search_rows = search and math.min(SEARCH_ROWS, math.max(3, height - 6)) or 0
  local band_rows = (status_rows() > 0 and 1 or 0) + (height >= FOOTER_MIN_ROWS and 1 or 0)
  local header_rows = height >= HEADER_MIN_ROWS and 1 or 0
  local content_rows = height - header_rows - search_rows - band_rows

  if width < setting("two_panel_min_cols", TWO_PANEL_MIN_COLS_DEFAULT) then
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 20, min = 20 }
    end
    columns[#columns + 1] = centre_column(ctx, content_rows)

    local right = right_panes(ctx)
    if #right > 0 and width >= setting("three_panel_min_cols", THREE_PANEL_MIN_COLS_DEFAULT) then
      local stack = {}
      for _, slot in ipairs(right) do
        stack[#stack + 1] = { slot = slot }
      end
      columns[#columns + 1] = { axis = "vertical", pct = 22, min = 30, children = stack }
    end
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
