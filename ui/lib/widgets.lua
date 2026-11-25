local hover = require("lib.hover")
local theme = require("lib.theme")

local widgets = {}

function widgets.panel(title, focused)
  local chrome = require("lib.chrome")
  local level = chrome.level(focused)
  return {
    title = chrome.label(title, level),
    borders = "all",
    padding = 0,
    border_type = chrome.border_type(level),
    border_style = chrome.border_style(level),
  }
end

function widgets.clamp(value, count)
  if count <= 0 then
    return 1
  end
  return math.max(1, math.min(value or 1, count))
end

function widgets.window(count, height, selected)
  if count <= 0 or height <= 0 then
    return 1, 0, false, false
  end
  if count <= height then
    return 1, count, false, false
  end
  selected = widgets.clamp(selected, count)
  local first = selected - math.floor(height / 2)
  first = math.max(1, math.min(first, count - height + 1))
  local last = first + height - 1
  return first, last, first > 1, last < count
end

---@param str string
---@return integer
function widgets.len(str)
  return text.width(str)
end

---@param str string
---@return integer
function widgets.chars(str)
  return utf8.len(str) or #str
end

function widgets.truncate(str, width)
  return text.truncate(str, width)
end

function widgets.truncate_hard(str, max)
  if max <= 1 and text.width(str) > max then
    return ""
  end
  return text.truncate(str, max)
end

function widgets.keep_left(str, max)
  return text.truncate(str, max, "")
end

function widgets.keep_right(str, max)
  return text.truncate(str, max, { ellipsis = "", side = "left" })
end

function widgets.middle_truncate(str, width)
  if width < 8 then
    return text.truncate(str, width)
  end
  return text.truncate(str, width, { side = "middle" })
end

function widgets.pad(str, width)
  return text.pad(str, width)
end

---@param spans string|table
---@return table[]
local function span_list(spans)
  if type(spans) == "string" then
    return { { text = spans } }
  end
  if spans.text ~= nil then
    return { spans }
  end
  return spans
end

local function overflow_marker(label)
  return { type = "text", len = 1, text = theme.dim(label), role = "overflow" }
end

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

    local marker = { text = is_selected and "▸ " or "  " }
    if is_selected then
      marker.style = { fg = theme.accent, bold = true }
    end

    local spans = { marker }
    for _, span in ipairs(span_list(row.spans)) do
      spans[#spans + 1] = span
    end

    local id = row.id or tostring(index)
    local style = nil
    if is_selected then
      style = opts.selected_style
    elseif row.hover ~= false and hover.id(id) then
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

function widgets.divider(width, char)
  return {
    type = "text",
    len = 1,
    text = theme.dim(string.rep(char or "─", math.max(0, width))),
  }
end

function widgets.hints(pairs_list)
  local spans = {}
  for _, pair in ipairs(pairs_list) do
    spans[#spans + 1] = { text = " " .. pair[1] .. " ", style = { bold = true } }
    spans[#spans + 1] = { text = pair[2] .. "  ", style = { fg = theme.muted } }
  end
  return { type = "text", len = 1, text = { spans } }
end

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

function widgets.now_ms()
  return (thurbox and thurbox.taken_at_ms) or 0
end

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
