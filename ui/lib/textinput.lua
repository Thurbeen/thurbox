local hover = require("lib.hover")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local textinput = {}

local function byte_at(text, n)
  if n <= 0 then
    return 1
  end
  return utf8.offset(text, n + 1) or (#text + 1)
end

local function split(field)
  local at = byte_at(field.value, field.cursor)
  return string.sub(field.value, 1, at - 1), string.sub(field.value, at)
end

function textinput.new(value)
  local text = value or ""
  return { value = text, cursor = widgets.chars(text) }
end

function textinput.set(field, value)
  field.value = value or ""
  field.cursor = widgets.chars(field.value)
  return field
end

function textinput.clear(field)
  return textinput.set(field, "")
end

function textinput.insert(field, text)
  if not text or text == "" then
    return field
  end
  local before, after = split(field)
  field.value = before .. text .. after
  field.cursor = field.cursor + widgets.chars(text)
  return field
end

local function characters(field)
  local chars = {}
  for _, code in utf8.codes(field.value) do
    chars[#chars + 1] = utf8.char(code)
  end
  local function separator(index)
    local char = chars[index]
    return char == nil or char == " " or char == "/" or char == "."
  end
  return chars, separator
end

local function word_start(field)
  local _, separator = characters(field)
  local at = field.cursor
  while at > 0 and separator(at) do
    at = at - 1
  end
  while at > 0 and not separator(at) do
    at = at - 1
  end
  return at
end

local function word_end(field)
  local chars, separator = characters(field)
  local at = field.cursor
  while at < #chars and separator(at + 1) do
    at = at + 1
  end
  while at < #chars and not separator(at + 1) do
    at = at + 1
  end
  return at
end

local function delete_between(field, from, to)
  local value = field.value
  field.value = string.sub(value, 1, byte_at(value, from) - 1)
    .. string.sub(value, byte_at(value, to))
  field.cursor = from
end

local function word_edit(field, key)
  local name = key.key
  if name == "left" or (key.alt and name == "b") then
    field.cursor = word_start(field)
  elseif name == "right" or (key.alt and name == "f") then
    field.cursor = word_end(field)
  elseif name == "backspace" then
    delete_between(field, word_start(field), field.cursor)
  elseif name == "delete" or (key.alt and name == "d") then
    delete_between(field, field.cursor, word_end(field))
  else
    return false
  end
  return true
end

local function control(field, key)
  local name = key.key
  if name == "a" then
    field.cursor = 0
  elseif name == "e" then
    field.cursor = widgets.chars(field.value)
  elseif name == "b" then
    field.cursor = math.max(0, field.cursor - 1)
  elseif name == "f" then
    field.cursor = math.min(widgets.chars(field.value), field.cursor + 1)
  elseif name == "d" then
    local before, after = split(field)
    if after ~= "" then
      field.value = before .. string.sub(after, (utf8.offset(after, 2) or (#after + 1)))
    end
  elseif name == "w" then
    local target = word_start(field)
    local before = string.sub(field.value, 1, byte_at(field.value, target) - 1)
    local _, after = split(field)
    field.value = before .. after
    field.cursor = target
  elseif name == "u" then
    local _, after = split(field)
    field.value = after
    field.cursor = 0
  elseif name == "k" then
    local before = split(field)
    field.value = before
  end
  return true
end

function textinput.key(field, key)
  if (key.ctrl or key.alt) and word_edit(field, key) then
    return true
  end
  if key.ctrl or key.alt then
    if key.ctrl and not key.alt and key.char and widgets.chars(key.char) == 1 then
      return control(field, key)
    end
    return false
  end

  local name = key.key
  if name == "backspace" then
    if field.cursor > 0 then
      local before, after = split(field)
      field.value = string.sub(before, 1, byte_at(before, field.cursor - 1) - 1) .. after
      field.cursor = field.cursor - 1
    end
    return true
  elseif name == "delete" then
    local before, after = split(field)
    if after ~= "" then
      field.value = before .. string.sub(after, (utf8.offset(after, 2) or (#after + 1)))
    end
    return true
  elseif name == "left" then
    field.cursor = math.max(0, field.cursor - 1)
    return true
  elseif name == "right" then
    field.cursor = math.min(widgets.chars(field.value), field.cursor + 1)
    return true
  elseif name == "home" then
    field.cursor = 0
    return true
  elseif name == "end" then
    field.cursor = widgets.chars(field.value)
    return true
  elseif name == "space" then
    textinput.insert(field, " ")
    return true
  end

  if key.char and not key.ctrl and widgets.chars(key.char) == 1 then
    textinput.insert(field, key.char)
    return true
  end
  return false
end

function textinput.node(field, opts)
  opts = opts or {}
  return {
    type = "input",
    len = 3,
    value = field.value or "",
    cursor = field.cursor or 0,
    placeholder = opts.placeholder or "",
    focused = opts.focused == true,
    style = { fg = theme.text },
    frame = {
      title = " " .. (opts.label or "") .. " ",
      borders = "all",
      border_style = { fg = hover.border(opts.focused, opts.id) },
    },
    id = opts.id,
    role = opts.id and "row" or nil,
  }
end

return textinput
