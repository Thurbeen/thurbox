-- The widget library.
--
-- THIS FILE IS THE POINT. The kernel ships four primitives — text, box, input,
-- surface — and nothing else. Lists, panels, gauges and dividers live here, in
-- Lua, composed from those four.
--
-- A prior attempt at v2 specified a library like this, never built it, and
-- watched its "frozen" 6-kind node catalog grow to 16 because every new
-- appearance had nowhere else to go. Each one cost a Rust enum variant, a
-- converter arm, a renderer arm, type definitions and a release. Everything
-- below costs a file save.
--
-- So: when you need a new appearance, add it HERE. Adding a node kind to the
-- kernel is a design decision, not a shortcut.

local hover = require("lib.hover")
local theme = require("lib.theme")

local widgets = {}

--- A bordered panel. Pass ctx.focused so the focused pane lights up.
function widgets.panel(title, focused)
  return {
    title = focused and (" ▸ " .. title .. " ") or (" " .. title .. " "),
    borders = "all",
    border_style = focused and theme.accent or theme.muted,
    padding = 0,
  }
end

--- Clamp `value` into [1, count]; returns 1 when the list is empty.
function widgets.clamp(value, count)
  if count <= 0 then
    return 1
  end
  return math.max(1, math.min(value or 1, count))
end

--- Which slice of `count` rows is visible in `height` rows, keeping `selected`
--- on screen.
---
--- This is the function that could not exist before the layout pass: it needs
--- the pane's RESOLVED height, which a plugin only has because the kernel
--- resolves rects before calling render.
---
--- Returns first, last (1-based, inclusive) and whether rows are hidden above
--- and below.
function widgets.window(count, height, selected)
  if count <= 0 or height <= 0 then
    return 1, 0, false, false
  end
  if count <= height then
    return 1, count, false, false
  end
  selected = widgets.clamp(selected, count)
  -- Centre the selection, then push the window back inside the list.
  local first = selected - math.floor(height / 2)
  first = math.max(1, math.min(first, count - height + 1))
  local last = first + height - 1
  return first, last, first > 1, last < count
end

--- Width in terminal COLUMNS — what every layout here budgets in.
---
--- Lua has no way to compute this: `#` counts bytes and `utf8.len` counts
--- codepoints, and a CJK glyph is one codepoint over two columns while a
--- combining mark is one codepoint over none. So the kernel measures, with the
--- same `unicode-width` the painter uses, and a budget computed here agrees
--- with what lands on the screen.
---
--- Use `widgets.chars` instead for a caret: `input.cursor` is a CHARACTER
--- offset, and columns would put it in the wrong place in exactly the text
--- this function exists for.
---@param str string
---@return integer
function widgets.len(str)
  return text.width(str)
end

--- Count of characters, for the one job columns cannot do: placing a caret.
---@param str string
---@return integer
function widgets.chars(str)
  return utf8.len(str) or #str
end

--- Truncate to `width` columns, marking the cut with an ellipsis.
function widgets.truncate(str, width)
  return text.truncate(str, width)
end

--- Truncate to `max` columns, marking the cut with an ellipsis — but return
--- NOTHING at `max <= 1` rather than a bare `…`.
---
--- v1's `ui::truncate_ellipsis`, and the counterpart to `widgets.truncate`
--- rather than a variant of it: the difference is what a one-column budget
--- means. For a NAME, `…` still says "there is a name here, it did not fit", so
--- `truncate` keeps it. For a status or a badge composed beside other segments,
--- a lone ellipsis carries nothing and costs the column something else could
--- have used, so this drops it.
---
--- Both bundled panes had grown their own copy of this, each with a comment
--- explaining why it was not `widgets.truncate`. That is the signal that the
--- library was missing a variant, not that the callers were wrong — so it lives
--- here now and the forks are gone.
function widgets.truncate_hard(str, max)
  if max <= 1 and text.width(str) > max then
    return ""
  end
  return text.truncate(str, max)
end

--- Keep the FIRST `max` columns, adding nothing.
---
--- What a left-aligned border title does when it overruns its area: ratatui
--- truncates the far side and marks the cut in no way at all. Use this when you
--- are MATCHING that behaviour; use `truncate`/`truncate_hard` when you are
--- telling the reader something was cut.
function widgets.keep_left(str, max)
  return text.truncate(str, max, "")
end

--- Keep the LAST `max` columns.
---
--- What a right-aligned border title does when it overruns: ratatui's
--- `Line::render_with_alignment` skips from the left, so the title shrinks
--- toward the right edge. v1's recorded frame shows exactly this — a 42-char
--- title in a 40-wide pane renders as `╭-osc52 (claude) …`.
function widgets.keep_right(str, max)
  return text.truncate(str, max, { ellipsis = "", side = "left" })
end

--- Truncate in the MIDDLE, keeping both ends.
---
--- For a path, the head says where you are and the leaf says which one it is, and
--- the leaf is the half an end-truncation throws away: a repo picker offering
--- `/home/me/.local/share/thurbox/worktrees/e854f81b/thurb` has spent every
--- column it had on boilerplate and cut off the only part that identifies the
--- repository. Falls back to end-truncation below eight columns, where there is
--- not room for two halves and an ellipsis.
function widgets.middle_truncate(str, width)
  if width < 8 then
    return text.truncate(str, width)
  end
  return text.truncate(str, width, { side = "middle" })
end

--- Pad `str` out to `width` columns.
function widgets.pad(str, width)
  return text.pad(str, width)
end

--- A row's text as a LIST of spans, whatever shape it arrived in.
---
--- A row may carry a bare string, one span table, or a list of them — three
--- spellings that were previously branched on at the point of use, where the
--- value's type had to be re-derived from whether it had a `text` field. Doing it
--- once here means the loop below sees one shape, and a type checker can follow
--- it (`lua-language-server` could not: it inferred the list and then flagged the
--- `.text` probe as an undefined field).
---@param spans string|table
---@return table[]
local function span_list(spans)
  if type(spans) == "string" then
    return { { text = spans } }
  end
  -- A single span carries `text`; a list does not.
  if spans.text ~= nil then
    return { spans }
  end
  return spans
end

--- An overflow marker: a row of its own, never drawn over one.
local function overflow_marker(label)
  return { type = "text", len = 1, text = theme.dim(label), role = "overflow" }
end

--- The visible window, plus which overflow markers fit beside it.
---
--- A marker is a row, so it costs a line the rows would otherwise have had —
--- and giving up that line can push a further row out of view, which calls for
--- the second marker too. Hence two passes rather than one subtraction.
---
--- What comes back is always a sub-range of `widgets.window` for the same
--- height, which is what lets a pane build spans against that wider window and
--- know the list reads no row it skipped.
---
--- A list with no line to spare — one or two rows with both ends hidden —
--- shows rows and no markers: a strip made entirely of "N more" says nothing.
local function marked_window(count, height, selected)
  if count <= height then
    return 1, count, false, false
  end
  for markers = 1, 2 do
    if height - markers < 1 then
      break
    end
    local first, last, above, below = widgets.window(count, height - markers, selected)
    if (above and 1 or 0) + (below and 1 or 0) <= markers then
      return first, last, above, below
    end
  end
  local first, last = widgets.window(count, height, selected)
  return first, last, false, false
end

--- A scrollable list of rows.
---
--- Each row is `{ spans = <text>, id =, class =, role = }` or a plain string.
--- Rows get identity by default (`role = "row"`), so decoration and event
--- targeting work without every pane opting in.
---
--- `len` and `fill` size the returned box. A caller that had to patch a size
--- onto the table after the call was reaching for the one prop the widget could
--- not express.
---
--- `selected_style` and `hover_style` are the row's own `style`, which the
--- kernel paints across its whole rect before the spans go on top. That is a
--- full-width bar with no spacer span to pad it and no style merged into every
--- span by hand — and a span that names its own colour keeps it, so a search
--- highlight stays visible under the bar. `hover_style` is matched on `row.id`
--- and skipped on the selected row, which already wears the stronger one.
---
--- opts: rows, selected, height, frame, empty, len, fill, selected_style,
--- hover_style
function widgets.list(opts)
  local rows = opts.rows or {}
  local height = opts.height or #rows
  local count = #rows
  local children = {}

  if count == 0 then
    children[1] = { type = "text", text = theme.dim(opts.empty or "  nothing here") }
    return {
      type = "box",
      frame = opts.frame,
      len = opts.len,
      fill = opts.fill,
      children = children,
    }
  end

  local selected = widgets.clamp(opts.selected, count)
  local first, last, more_above, more_below = marked_window(count, height, selected)

  -- The markers bracket the rows and their counts are exact: everything before
  -- `first` and everything after `last` is hidden, and no row is drawn over.
  if more_above then
    children[1] = overflow_marker("  ↑ " .. (first - 1) .. " more")
  end
  for index = first, last do
    local row = rows[index]
    if type(row) == "string" then
      row = { spans = row }
    end
    local is_selected = index == selected
    local classes = { "row" }
    if row.class then
      classes[#classes + 1] = row.class
    end
    if is_selected then
      classes[#classes + 1] = "selected"
    end

    -- The selection marker is part of the row, so a row is one line and the
    -- window arithmetic above stays exact.
    local marker = { text = is_selected and "▸ " or "  " }
    if is_selected then
      marker.style = { fg = theme.accent, bold = true }
    end

    local spans = { marker }
    for _, span in ipairs(span_list(row.spans)) do
      spans[#spans + 1] = span
    end

    -- Addressable by default. `index` is the row's position in the FULL list,
    -- not the visible window, so a click resolves to the same index j/k moves
    -- through -- which is what lets a pane answer a click with one line instead
    -- of repeating the window arithmetic. Hover is matched on the very id the
    -- node carries, so the highlight and the click can never disagree.
    local id = row.id or tostring(index)
    local style = nil
    if is_selected then
      style = opts.selected_style
    elseif hover.id(id) then
      style = opts.hover_style
    end

    children[#children + 1] = {
      type = "text",
      len = 1,
      text = { spans },
      style = style,
      id = id,
      class = table.concat(classes, " "),
      role = "row",
    }
  end

  if more_below then
    children[#children + 1] = overflow_marker("  ↓ " .. (count - last) .. " more")
  end

  return {
    type = "box",
    frame = opts.frame,
    len = opts.len,
    fill = opts.fill,
    children = children,
  }
end

--- A horizontal bar. Composed from text — not a node kind.
function widgets.gauge(ratio, opts)
  opts = opts or {}
  local width = math.max(0, opts.width or 10)
  ratio = math.max(0, math.min(ratio or 0, 1))
  local filled = math.floor(ratio * width + 0.5)
  return {
    type = "text",
    len = 1,
    text = {
      {
        { text = string.rep("█", filled), style = opts.style or theme.accent },
        { text = string.rep("░", width - filled), style = theme.muted },
        { text = opts.label and (" " .. opts.label) or "" },
      },
    },
  }
end

--- A horizontal rule. Also just text.
function widgets.divider(width, char)
  return {
    type = "text",
    len = 1,
    text = theme.dim(string.rep(char or "─", math.max(0, width))),
  }
end

--- A one-line key hint strip: { {"j/k", "move"}, {"enter", "open"} }
function widgets.hints(pairs_list)
  local spans = {}
  for _, pair in ipairs(pairs_list) do
    spans[#spans + 1] = { text = " " .. pair[1] .. " ", style = { bold = true } }
    spans[#spans + 1] = { text = pair[2] .. "  ", style = { fg = theme.muted } }
  end
  return { type = "text", len = 1, text = { spans } }
end

--- Index of the row whose identity is `id`, or nil when no row carries it.
---
--- The find-the-clicked-row loop, shared: a click hands back the id the row
--- was drawn with, and every pane answers it by scanning the same rows it
--- rendered. `key` names the field carrying identity (default `"id"`), or is
--- a function for rows whose identity is nested.
function widgets.index_of(rows, id, key)
  for index, row in ipairs(rows) do
    local value
    if type(key) == "function" then
      value = key(row)
    else
      value = row[key or "id"]
    end
    if value == id then
      return index
    end
  end
  return nil
end

--- Epoch milliseconds the frame is being drawn for.
---
--- v1's formatters read the wall clock; a plugin has no clock (the stdlib ships
--- no `os`), so ages are measured against the snapshot's own instant — which is
--- the moment the rows being drawn were read.
function widgets.now_ms()
  return (thurbox and thurbox.taken_at_ms) or 0
end

--- v1's `format_time_ago`, including its saturating subtraction: a timestamp in
--- the future reads `0s ago` rather than a negative age. `millis` and `now` are
--- both epoch milliseconds; `now` defaults to the snapshot's instant.
function widgets.time_ago(millis, now)
  local elapsed = math.floor(math.max(0, (now or widgets.now_ms()) - millis) / 1000)
  if elapsed < 60 then
    return elapsed .. "s ago"
  elseif elapsed < 3600 then
    return math.floor(elapsed / 60) .. "m ago"
  elseif elapsed < 86400 then
    return math.floor(elapsed / 3600) .. "h ago"
  end
  return math.floor(elapsed / 86400) .. "d ago"
end

return widgets
