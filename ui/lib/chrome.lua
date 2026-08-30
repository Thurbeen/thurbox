-- Hand-drawn border chrome.
--
-- Two bundled panes draw their borders BY HAND rather than with a kernel
-- `frame`, because a frame's title is a plain, unstyled, left-aligned string
-- and cannot express what v1 puts on a border: the focused title *badge*
-- (inverted fg on an accent background), a right-aligned styled title, the
-- per-session status-dot strip, scroll counts overlaid on the border cells, or
-- a scrollbar in the border column. Composing the border out of `text` nodes
-- costs nothing — the border rows are the rows a frame would have occupied —
-- and stays inside the four-kind vocabulary.
--
-- This module is the half those panes must agree on: the span/cell primitives
-- border segments are layered with, the focus styling both map the same three
-- levels through, and the framed-pane builder the terminal pane composes its
-- chrome from. What stays in each pane is its own layout decisions.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

local chrome = {}

-- ── Span measurement ────────────────────────────────────────────────────────

--- Character count across a span list (widgets.len handles one string).
function chrome.spans_len(spans)
  local total = 0
  for _, span in ipairs(spans) do
    total = total + widgets.len(span.text or "")
  end
  return total
end

-- ── Cell buffers ────────────────────────────────────────────────────────────
--
-- A border row is built as a cell buffer so segments can be *layered* the way
-- v1 layers them: the dot strip is a right-aligned title, and the scroll count
-- is painted over the same cells afterwards.

function chrome.new_cells(width, char, style)
  local cells = {}
  for index = 1, width do
    cells[index] = { ch = char, style = style }
  end
  return cells
end

--- Paint `text` into the buffer starting at `at` (1-based). Out-of-range cells
--- are dropped, which is how an over-long title clips at either edge.
function chrome.place_text(cells, at, text, style)
  local index = at
  for _, code in utf8.codes(text or "") do
    if index >= 1 and index <= #cells then
      cells[index] = { ch = utf8.char(code), style = style }
    end
    index = index + 1
  end
  return index
end

function chrome.place_spans(cells, at, spans)
  local index = at
  for _, span in ipairs(spans) do
    index = chrome.place_text(cells, index, span.text, span.style)
  end
  return index
end

--- Coalesce the buffer back into spans. Styles are compared by identity, which
--- is exact here because every segment is painted with one shared style table.
---
--- Each run's characters accumulate in a buffer and concatenate once at flush:
--- a border row is mostly one long run of `─`, and appending to a string per
--- cell was O(width²) byte copying — ~200 intermediate strings per border at
--- 200 columns, twice per render.
function chrome.cells_to_spans(cells)
  local spans, buffer, current_style = {}, nil, nil
  local function flush()
    if buffer then
      spans[#spans + 1] = { text = table.concat(buffer), style = current_style }
    end
  end
  for _, cell in ipairs(cells) do
    if buffer and cell.style == current_style then
      buffer[#buffer + 1] = cell.ch
    else
      flush()
      buffer = { cell.ch }
      current_style = cell.style
    end
  end
  flush()
  return spans
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

-- ── The framed pane ─────────────────────────────────────────────────────────

chrome.ROUNDED = { tl = "╭", tr = "╮", bl = "╰", br = "╯", h = "─", v = "│" }
chrome.SQUARE = { tl = "┌", tr = "┐", bl = "└", br = "┘", h = "─", v = "│" }

--- The top border as a NODE, not a run list.
---
--- Identity is per node, so a chip painted as one span among many can never be
--- a click target however it is styled. When any run carries a `role`, the row
--- becomes a horizontal box of one text node per run, each with an exact `len`
--- so the geometry is bit-identical to the single-node form.
local function top_row_node(runs)
  local clickable = false
  for _, run in ipairs(runs) do
    if run.role then
      clickable = true
      break
    end
  end
  if not clickable then
    return { type = "text", len = 1, text = { runs } }
  end

  local children = {}
  for _, run in ipairs(runs) do
    local width = widgets.len(run.text)
    if width > 0 then
      children[#children + 1] = {
        type = "text",
        len = width,
        role = run.role,
        text = { { { text = run.text, style = run.style } } },
      }
    end
  end
  return { type = "box", axis = "horizontal", len = 1, children = children }
end

--- A bordered pane whose title can be right-aligned and styled, and whose right
--- border column can be replaced row-by-row (that is where a scrollbar goes).
---
--- opts: width, height, title, title_style, title_align, border_style, square,
---       left (runs painted over the top border's left half),
---       right_column (runs, one per inner row),
---       right_column_role (identity for that column, so it can be clicked),
---       body (node)
function chrome.frame(opts)
  local width, height = opts.width or 0, opts.height or 0
  if width < 2 or height < 2 then
    return opts.body
  end

  local set = opts.square and chrome.SQUARE or chrome.ROUNDED
  local border = opts.border_style
  local inner_w, inner_h = width - 2, height - 2

  local title = opts.title or ""
  local top
  if opts.title_align == "right" then
    -- The strip is painted first and the title fills what it leaves, so the two
    -- share one row without either being drawn over the other.
    local left, left_w = opts.left or {}, 0
    for _, run in ipairs(left) do
      left_w = left_w + widgets.len(run.text)
    end
    title = widgets.keep_right(title, math.max(0, inner_w - left_w))
    top = { { text = set.tl, style = border } }
    for _, run in ipairs(left) do
      top[#top + 1] = run
    end
    top[#top + 1] = {
      text = string.rep(set.h, math.max(0, inner_w - left_w - widgets.len(title))),
      style = border,
    }
    top[#top + 1] = { text = title, style = opts.title_style }
    top[#top + 1] = { text = set.tr, style = border }
  else
    title = widgets.keep_left(title, inner_w)
    top = {
      { text = set.tl, style = border },
      { text = title, style = opts.title_style },
      { text = string.rep(set.h, inner_w - widgets.len(title)), style = border },
      { text = set.tr, style = border },
    }
  end

  local edge = { text = set.v, style = border }
  --- One border column, `inner_h` rows tall.
  ---
  --- `role` is what makes it a click target: identity is per node, and this is
  --- one node for the whole column, so a press arrives with `y` already being
  --- the row of the bar it landed on and `h` the length of the bar. A column
  --- with no role is inert chrome, which is what the left edge is.
  local function column(rows, role)
    local lines = {}
    for row = 1, inner_h do
      lines[row] = { (rows and rows[row]) or edge }
    end
    return { type = "text", len = 1, role = role, text = lines }
  end

  return {
    type = "box",
    axis = "vertical",
    children = {
      top_row_node(top),
      {
        type = "box",
        axis = "horizontal",
        fill = 1,
        children = {
          column(nil),
          opts.body,
          column(opts.right_column, opts.right_column and opts.right_column_role),
        },
      },
      {
        type = "text",
        len = 1,
        text = {
          {
            { text = set.bl, style = border },
            { text = string.rep(set.h, inner_w), style = border },
            { text = set.br, style = border },
          },
        },
      },
    },
  }
end

return chrome
