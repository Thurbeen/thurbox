-- What the two full-height panes agree on about their borders.
--
-- The borders themselves are ordinary kernel `frame`s. They were once drawn by
-- hand, out of `text` nodes and a cell buffer, because a frame's title was a
-- plain unstyled left-aligned string and there was no way to put anything else
-- on a border cell. A frame now takes styled title runs, a `title_align`, a
-- `border_type` and an `overlay` — the strip, the scroll counts and the
-- scrollbar all paint onto the border cells the block drew — so the cell buffer
-- and the framed-pane builder are gone and what is left here is the agreement:
-- the three focus levels both panes map through, and the box-drawing set the
-- agent pane's hint box still draws by hand because it is a box INSIDE a pane,
-- not a frame around one.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

local chrome = {}

-- ── Span measurement ────────────────────────────────────────────────────────

--- Display columns across a span list (widgets.len handles one string).
function chrome.spans_len(spans)
  local total = 0
  for _, span in ipairs(spans) do
    total = total + widgets.len(span.text or "")
  end
  return total
end

-- ── Focus styling ───────────────────────────────────────────────────────────
--
-- v1's three levels (`ui::FocusLevel`). The kernel publishes a single `focused`
-- boolean, and it is the PANE that knows what its unfocused state means — the
-- same split v1 makes, deciding per pane in `view.rs` rather than in the
-- widget. Both bundled panes answer `active`: the session list always shows
-- the current session, and the terminal pane stays the centre of attention, so
-- each keeps the plain accent border with focus elsewhere. `inactive` (the
-- gray border) is therefore unreached today. It is kept because it is not
-- dead: v1 uses it while another context owns the centre, so the pane that
-- returns brings the level back with it.

function chrome.border_style(level)
  if level == "focused" then
    return { fg = theme.accent_bright }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

--- v1's `ui::title_style` / `Theme::focused_title()`. Focused is a BADGE —
--- inverted foreground on an accent field, bold — not merely a brighter text
--- colour.
function chrome.title_style(level)
  if level == "focused" then
    return { fg = theme.role("inverted_fg"), bg = theme.accent, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

--- The square box-drawing set, for a box a pane draws INSIDE itself out of
--- text — where there is no frame to ask for `border_type = "square"`.
chrome.SQUARE = { tl = "┌", tr = "┐", bl = "└", br = "┘", h = "─", v = "│" }

return chrome
