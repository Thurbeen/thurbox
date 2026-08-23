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
--- resolves rects before calling render (design.md D2).
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

--- Length in characters, not bytes.
---
--- Lua's `#` is a byte count, so "Rosé Pine" measures 10 and pads a column
--- short. This is still not display *width* — a CJK or emoji character occupies
--- two columns and counts as one here — but it is right for the Latin text the
--- bundled panes show. True width belongs in the kernel, which already has
--- unicode-width linked.
function widgets.len(text)
  return utf8 and utf8.len(text) or #text
end

--- Byte offset of the character at position `n`, for safe substring cuts.
local function offset(text, n)
  if not utf8 then
    return n
  end
  return (utf8.offset(text, n + 1) or (#text + 1)) - 1
end

--- Truncate to `width` characters, marking the cut with an ellipsis.
function widgets.truncate(text, width)
  if width <= 0 then
    return ""
  end
  if widgets.len(text) <= width then
    return text
  end
  if width == 1 then
    return "…"
  end
  return string.sub(text, 1, offset(text, width - 1)) .. "…"
end

--- Truncate to `max` characters, marking the cut with an ellipsis — but return
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
function widgets.truncate_hard(text, max)
  if widgets.len(text) <= max then
    return text
  end
  if max <= 1 then
    return ""
  end
  return string.sub(text, 1, offset(text, max - 1)) .. "…"
end

--- Keep the FIRST `max` characters, adding nothing.
---
--- What a left-aligned border title does when it overruns its area: ratatui
--- truncates the far side and marks the cut in no way at all. Use this when you
--- are MATCHING that behaviour; use `truncate`/`truncate_hard` when you are
--- telling the reader something was cut.
function widgets.keep_left(text, max)
  if max <= 0 then
    return ""
  end
  if widgets.len(text) <= max then
    return text
  end
  return string.sub(text, 1, offset(text, max))
end

--- Keep the LAST `max` characters.
---
--- What a right-aligned border title does when it overruns: ratatui's
--- `Line::render_with_alignment` skips from the left, so the title shrinks
--- toward the right edge. v1's recorded frame shows exactly this — a 42-char
--- title in a 40-wide pane renders as `╭-osc52 (claude) …`.
function widgets.keep_right(text, max)
  if max <= 0 then
    return ""
  end
  local count = widgets.len(text)
  if count <= max then
    return text
  end
  return string.sub(text, offset(text, count - max) + 1)
end

--- Truncate in the MIDDLE, keeping both ends.
---
--- For a path, the head says where you are and the leaf says which one it is, and
--- the leaf is the half an end-truncation throws away: a repo picker offering
--- `/home/me/.local/share/thurbox/worktrees/e854f81b/thurb` has spent every
--- column it had on boilerplate and cut off the only part that identifies the
--- repository. Falls back to end-truncation below eight columns, where there is
--- not room for two halves and an ellipsis.
function widgets.middle_truncate(text, width)
  if width <= 0 then
    return ""
  end
  if widgets.len(text) <= width then
    return text
  end
  if width < 8 then
    return widgets.truncate(text, width)
  end
  -- The tail gets the larger share of an odd remainder: it carries the leaf.
  local keep = width - 1
  local head = math.floor(keep / 2)
  local tail = keep - head
  local len = widgets.len(text)
  return string.sub(text, 1, offset(text, head))
    .. "…"
    .. string.sub(text, offset(text, len - tail) + 1)
end

--- Pad `text` out to `width` characters.
function widgets.pad(text, width)
  local short = width - widgets.len(text)
  if short <= 0 then
    return text
  end
  return text .. string.rep(" ", short)
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

--- A scrollable list of rows.
---
--- Each row is `{ spans = <text>, id =, class =, role = }` or a plain string.
--- Rows get identity by default (`role = "row"`), so decoration and event
--- targeting work without every pane opting in.
---
--- opts: rows, selected, height, focused, frame, empty
function widgets.list(opts)
  local rows = opts.rows or {}
  local height = opts.height or #rows
  local count = #rows

  if count == 0 then
    return {
      type = "box",
      frame = opts.frame,
      children = { { type = "text", text = theme.dim(opts.empty or "  nothing here") } },
    }
  end

  local selected = widgets.clamp(opts.selected, count)
  local first, last, more_above, more_below = widgets.window(count, height, selected)

  local children = {}
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

    children[#children + 1] = {
      type = "text",
      len = 1,
      text = { spans },
      -- Addressable by default. `index` is the row's position in the FULL
      -- list, not the visible window, so a click resolves to the same index
      -- j/k moves through -- which is what lets a pane answer a click with
      -- one line instead of repeating the window arithmetic.
      id = row.id or tostring(index),
      class = table.concat(classes, " "),
      role = "row",
    }
  end

  -- Overflow markers, so a windowed list never silently hides rows.
  if more_above then
    children[1] = {
      type = "text",
      len = 1,
      text = theme.dim("  ↑ " .. (first - 1) .. " more"),
      role = "overflow",
    }
  end
  if more_below then
    children[#children] = {
      type = "text",
      len = 1,
      text = theme.dim("  ↓ " .. (count - last) .. " more"),
      role = "overflow",
    }
  end

  return { type = "box", frame = opts.frame, children = children }
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

--- The spinner glyph for this frame.
---
--- The 8 is the kernel's `ANIMATION_HZ` (`kernel::host`), and the two must
--- stay in lockstep: the shared animation clock invalidates `pure` panes at
--- that rate and only while something is animating, so a spinner advancing
--- faster than the clock would skip frames and one advancing slower would be
--- re-rendered for no visible change.
function widgets.spinner(elapsed)
  local frame = math.floor((elapsed or 0) * 8) % #theme.spinner + 1
  return theme.spinner[frame]
end

--- The animated glyph for a session status.
function widgets.status_glyph(status, elapsed)
  local spec = theme.status(status)
  local glyph = spec.glyph
  if status == "working" then
    glyph = widgets.spinner(elapsed)
  end
  return { text = glyph, style = { fg = spec.color } }
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
