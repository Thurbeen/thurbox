-- What the full-height panes agree on about their borders.
--
-- The borders themselves are ordinary kernel `frame`s. They were once drawn by
-- hand, out of `text` nodes and a cell buffer, because a frame's title was a
-- plain unstyled left-aligned string and there was no way to put anything else
-- on a border cell. A frame now takes styled title runs, a `title_align`, a
-- `border_type` and an `overlay` — the strip, the scroll counts and the
-- scrollbar all paint onto the border cells the block drew — so the cell buffer
-- and the framed-pane builder are gone and what is left here is the agreement:
-- how a pane's border and title say whether it has focus — reached through
-- `lib/ui`'s `ui.panel` for a pane that uses the component layer, and directly by
-- the agent pane, which still assembles its own frame — and the box-drawing set
-- the agent pane's hint box draws by hand because it is a box INSIDE a pane, not
-- a frame around one.

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
-- One question every pane has to answer the same way: which one has the keys?
-- The answer is carried three ways at once, so it survives any one of them
-- being lost:
--
--   * the SHAPE of the border — thick for the focused pane, thin for the rest;
--   * a MARK and a BADGE on the title — ` ▸ Title `, bold, on a filled field;
--   * COLOUR — the theme's `border_focused` and `border_unfocused` roles.
--
-- The first two are what a monochrome terminal, a colour-blind reader and a
-- low-contrast theme are left with, which is why colour is the last of the
-- three rather than the only one. v1 drew the focused border thick as well.
--
-- The kernel publishes a single `focused` boolean and `chrome.level` maps it
-- to one of three levels. Unfocused is `inactive`: a quiet border in the
-- theme's own unfocused role, so the one accented frame on screen is the one
-- with focus. `active` (thin, accent) is kept for a pane that wants to stay
-- lit without focus — an edited pane from an older release asks for it by
-- name — but no bundled pane does: an accent on two frames at once is what
-- made "which one has focus?" a question in the first place.

--- The mark a focused pane's title carries. U+25B8, a narrow glyph in every
--- East Asian width table, so it costs one column wherever it is drawn.
chrome.MARK = "▸"

--- The level a pane's `ctx.focused` maps to.
---@param focused boolean?
---@return "focused"|"inactive"
function chrome.level(focused)
  return focused and "focused" or "inactive"
end

--- The border glyphs a level is drawn with: a `frame.border_type`.
function chrome.border_type(level)
  return level == "focused" and "thick" or "rounded"
end

--- The horizontal border glyph a level is drawn with, for a pane that paints
--- runs over its own top border and has to fill the gaps between them.
function chrome.rule(level)
  return level == "focused" and "━" or "─"
end

function chrome.border_style(level)
  if level == "focused" then
    return { fg = theme.border_focused, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

--- v1's `ui::title_style` / `Theme::focused_title()`. Focused is a BADGE —
--- inverted foreground on the focused border's colour, bold — not merely a
--- brighter text colour, so the title and the frame around it agree.
function chrome.title_style(level)
  if level == "focused" then
    return { fg = theme.role("inverted_fg"), bg = theme.border_focused, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.secondary }
end

--- A title's text with its padding, and the mark when the level is focused.
---@param text string
---@param level string
---@return string
function chrome.label(text, level)
  if level == "focused" then
    return " " .. chrome.MARK .. " " .. text .. " "
  end
  return " " .. text .. " "
end

--- The whole focus convention as a `frame`: title, border glyphs and border
--- colour. `ui.panel` builds on it; a pane that assembles its own frame can
--- start from it and add what it needs.
---@param title string
---@param level string
---@return thurbox.Frame
function chrome.frame(title, level)
  return {
    title = { { text = chrome.label(title, level), style = chrome.title_style(level) } },
    border_type = chrome.border_type(level),
    border_style = chrome.border_style(level),
  }
end

--- The square box-drawing set, for a box a pane draws INSIDE itself out of
--- text — where there is no frame to ask for `border_type = "square"`.
chrome.SQUARE = { tl = "┌", tr = "┐", bl = "└", br = "┘", h = "─", v = "│" }

return chrome
