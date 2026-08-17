-- The arrangement.
--
-- v1 hardcoded this in Rust (`ui::layout::compute_layout`) with its breakpoints
-- baked into the binary. Here it is a function of the available size, so the
-- thresholds below are yours to change — edit, save, and the next frame uses
-- them.
--
-- It only ARRANGES. It never calls a plugin, so a mistake here cannot break one
-- (and a broken plugin cannot break this).
--
-- It runs BEFORE any plugin renders, which is what lets the kernel tell each
-- plugin the rect it is drawing into — and why the open/closed state of a
-- column lives in `lib.panels` rather than inside the plugin that fills it.
--
-- The interface is two panes between three chrome bands:
--
--   ┌─────────────────────────────────────────┐ header  (kernel)
--   ├──────────┬──────────────────────────────┤
--   │ sessions │           center             │
--   │  (25%)   │        (remainder)           │
--   ├──────────┴──────────────────────────────┤
--   │ status                                  │ only when there is a message
--   ├─────────────────────────────────────────┤
--   │ footer                                  │ (kernel)
--   └─────────────────────────────────────────┘
--
-- `header`, `status` and `footer` are BANDS: the kernel fills them, this file
-- decides whether and where they appear. Move one, reorder them, or delete a
-- line to drop it — the kernel neither knows nor minds. What a band CONTAINS is
-- not arrangement, so it is not decided here.
--
-- v1's other columns (info, tasks, files) were removed along with their
-- plugins; their slots went too, because a slot no plugin can fill is a rect
-- reserved for nothing. Re-adding a pane means adding its slot back here — which
-- is the whole reason the arrangement is a file you can edit rather than a
-- layout compiled into the binary. `search` is the first one back.
--
-- The centre pane is never dropped: it takes whatever the session column leaves.
-- That is the point of the split — v1 was built to avoid swapping between panes,
-- so the list SITS BESIDE the terminal rather than replacing it.
--
-- The status band costs nothing while it is quiet: the kernel draws nothing into
-- it when there is no message, and the row it would occupy is only carved when
-- `status_rows()` below says there is something to say.

local panels = require("lib.panels")

-- v1 `Settings::two_panel_min_cols`, and now the user's own value.
--
-- The fallback is not decoration: the arrangement runs before anything else and
-- must never fail closed, so a kernel that published nothing still gets v1's
-- default rather than a screen with no panes on it.
local TWO_PANEL_MIN_COLS_DEFAULT = 80

local function two_panel_min_cols()
  local settings = thurbox and thurbox.settings
  return (settings and settings.two_panel_min_cols) or TWO_PANEL_MIN_COLS_DEFAULT
end

-- v1 `split_vertical`: below 20 rows the header is dropped so the panes get
-- every row there is. The bands are surrendered identity-first — it is the least
-- urgent row on screen — and the message band last, because a hidden error is
-- the failure this chrome exists to prevent.
local HEADER_MIN_ROWS = 20
local FOOTER_MIN_ROWS = 4

-- Rows the search strip asks for: its frame, the query, a group header and a
-- handful of results. Clamped against the terminal below, so a short screen
-- gives it less rather than losing the panes underneath it.
local SEARCH_ROWS = 12

--- Does the status band have anything to say?
---
--- The kernel publishes this rather than the message itself: what a band shows
--- is not the arrangement's business, but whether it needs a row is.
local function status_rows()
  return (thurbox and thurbox.chrome and thurbox.chrome.status_rows) or 0
end

--- Will a loaded plugin actually paint into `slot`?
---
--- Removing a pane takes two switches and they are not the same kind of thing:
--- the panel toggle below is ARRANGEMENT state (F9, mine to decide), while the
--- disabled set is DELIVERY state (settings → Interface → space, the kernel's).
--- Reserving a rect needs both to agree, because a slot no plugin can fill is a
--- rect reserved for nothing — the same reason v1's info/tasks/files slots left
--- with their plugins. Consulting only the toggle is what left a turned-off
--- session list as an empty quarter-width column.
---
--- Only optional PLUGIN columns are gated on this. The chrome bands are drawn by
--- the kernel and belong to no plugin, and `center` is never dropped, so neither
--- appears in `ctx.slots` and neither may ask.
---
--- Unknown answers count as filled. The arrangement runs before anything else
--- and must never fail closed: a kernel that published no slot table at all
--- should still get the interface it had, not a screen with a pane missing.
local function filled(ctx, slot)
  local slots = ctx and ctx.slots
  if type(slots) ~= "table" then
    return true
  end
  return slots[slot] == true
end

return function(ctx)
  local height = ctx.height or 0
  local children = {}

  if height >= HEADER_MIN_ROWS then
    children[#children + 1] = { slot = "header", len = 1 }
  end

  -- Below 80 columns there is only room for the agent: v1 returns a
  -- `PanelAreas` with every column `None` and the terminal taking `content`.
  if (ctx.width or 0) < two_panel_min_cols() then
    children[#children + 1] = { slot = "center" }
  else
    local columns = {}
    -- The session column starts open; F9 hides it so the terminal can have the
    -- full width (v1's `Action::ToggleSessionList`). `filled` is the other half:
    -- turned off in the Interface tab, the column is not reserved either.
    if panels.shown("sessions") and filled(ctx, "sessions") then
      columns[#columns + 1] = { slot = "sessions", pct = 25, min = 20 }
    end
    columns[#columns + 1] = { slot = "center" }
    children[#children + 1] = { axis = "horizontal", children = columns }
  end

  -- Search is a full-width STRIP above the bands rather than a float, and that
  -- is the point of it: it highlights matches inside the panes it is searching,
  -- so a modal covering them would hide the thing it is pointing at. It shrinks
  -- the content the same way a side column does — v1 carves the same row in
  -- `compute_layout` and for the same reason.
  if panels.shown("search") and filled(ctx, "search") then
    children[#children + 1] =
      { slot = "search", len = math.min(SEARCH_ROWS, math.max(3, height - 6)) }
  end

  -- The message band takes a row only while there is a message, so a quiet
  -- interface is not paying a row to say nothing.
  if status_rows() > 0 then
    children[#children + 1] = { slot = "status", len = 1 }
  end
  if height >= FOOTER_MIN_ROWS then
    children[#children + 1] = { slot = "footer", len = 1 }
  end

  return { children = children }
end
