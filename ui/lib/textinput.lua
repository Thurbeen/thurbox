-- A text field: a buffer, a cursor, and the keys that move them.
--
-- The `input` node renders a value and a caret; it owns no buffer. That is the
-- right split — a hot reload must not lose what you have typed, and `state` is
-- what survives one — but it leaves every pane with a text field to implement
-- editing. So it lives here once, exactly as v1 keeps its own in one place
-- (`modals::apply_ctrl_line_edit` over a `LineEdit` trait) rather than per
-- modal.
--
-- A field is a PLAIN TABLE, `{ value = "", cursor = 0 }`, because that is what
-- `state` can persist: reading `state.foo` builds a fresh Lua table, so a
-- caller mutates the table it holds and writes the whole thing back. Every
-- function here therefore mutates in place and returns whether it consumed the
-- key.
--
-- `cursor` counts CHARACTERS before the caret, not bytes — the kernel's caret is
-- placed by character offset, and a path with an accent in it must not put the
-- caret in the wrong column.
--
-- One chord v1 has is missing, and cannot be had: `ctrl+h` (delete backwards) is
-- reserved by the kernel for moving focus, so it never reaches a plugin.
-- `backspace` does the same thing and is what a keyboard sends anyway.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

local textinput = {}

--- Byte offset just before character `n` (1-based), for slicing.
local function byte_at(text, n)
  if n <= 0 then
    return 1
  end
  return utf8.offset(text, n + 1) or (#text + 1)
end

--- The characters before and after the caret.
local function split(field)
  local at = byte_at(field.value, field.cursor)
  return string.sub(field.value, 1, at - 1), string.sub(field.value, at)
end

function textinput.new(value)
  local text = value or ""
  return { value = text, cursor = widgets.len(text) }
end

--- Replace the contents, caret at the end — how a field is prefilled.
function textinput.set(field, value)
  field.value = value or ""
  field.cursor = widgets.len(field.value)
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
  field.cursor = field.cursor + widgets.len(text)
  return field
end

--- Start of the word before the caret, in characters — the target `ctrl+w`
--- deletes back to. Trailing separators are skipped first, so deleting a word
--- from `~/src/thing/` lands on `thing/` rather than on nothing.
local function word_start(field)
  local at = field.cursor
  local chars = {}
  for _, code in utf8.codes(field.value) do
    chars[#chars + 1] = utf8.char(code)
  end
  local function separator(index)
    local char = chars[index]
    return char == nil or char == " " or char == "/" or char == "."
  end
  while at > 0 and separator(at) do
    at = at - 1
  end
  while at > 0 and not separator(at) do
    at = at - 1
  end
  return at
end

--- The readline chords v1's own fields accept, and nothing else.
---
--- Every `ctrl`+letter is consumed whether or not it is mapped, so a bare
--- control character can never leak into the value — v1 makes the same choice,
--- for the same reason.
local function control(field, key)
  local name = key.key
  if name == "a" then
    field.cursor = 0
  elseif name == "e" then
    field.cursor = widgets.len(field.value)
  elseif name == "b" then
    field.cursor = math.max(0, field.cursor - 1)
  elseif name == "f" then
    field.cursor = math.min(widgets.len(field.value), field.cursor + 1)
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

--- Offer a key to the field. True when it was consumed.
---
--- Deliberately does NOT consume `enter`, `esc` or `tab`: those belong to
--- whatever the field is inside, which is the only thing that knows what
--- submitting means.
function textinput.key(field, key)
  if key.ctrl or key.alt then
    -- A modifier chord that is not a line edit is still swallowed rather than
    -- typed, but only for letters: `ctrl+p` and friends belong to the pane, and
    -- it declares them as actions so they never arrive here at all.
    if key.ctrl and not key.alt and key.char and widgets.len(key.char) == 1 then
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
    field.cursor = math.min(widgets.len(field.value), field.cursor + 1)
    return true
  elseif name == "home" then
    field.cursor = 0
    return true
  elseif name == "end" then
    field.cursor = widgets.len(field.value)
    return true
  elseif name == "space" then
    textinput.insert(field, " ")
    return true
  end

  -- Anything that produced a character is typed, which is what makes a path
  -- with `~`, `/` or a digit in it work without listing them.
  if key.char and not key.ctrl and widgets.len(key.char) == 1 then
    textinput.insert(field, key.char)
    return true
  end
  return false
end

--- The field as a node: a framed one-line input, its label the frame's title.
---
--- `focused` drives the border AND the caret: the kernel paints the caret in the
--- one input that claims it, so a screen holding two fields puts it in the one
--- being typed into rather than in whichever holds text.
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
      border_style = { fg = opts.focused and theme.border_focused or theme.border },
    },
    id = opts.id,
    role = opts.id and "row" or nil,
  }
end

return textinput
