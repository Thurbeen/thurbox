local modal = require("lib.modal")
local textinput = require("lib.textinput")
local theme = require("lib.theme")

local function pending()
  local ask = store.rename
  if type(ask) ~= "table" or type(ask.session) ~= "string" then
    return nil
  end
  return ask
end

local function field_of(ask)
  return ask.field or textinput.new(ask.name)
end

return {
  name = "rename",
  ui_state = function()
    return { open = pending() ~= nil }
  end,
  slot = "float",
  order = 62,
  floats = true,
  pure = true,
  focusable = false,
  events = { "command.done", "command.failed" },

  render = function(_)
    local ask = pending()
    if not ask then
      return { type = "text", text = "" }
    end
    local note, note_style = "", { fg = theme.muted }
    if ask.error then
      note, note_style = " " .. ask.error, { fg = theme.bad }
    elseif ask.waiting then
      note = " renaming…"
    end
    return modal.frame("Rename session", {
      cols = 72,
      rows = 7,
      children = {
        textinput.node(field_of(ask), { label = "Name", focused = not ask.waiting }),
        { type = "text", len = 1, text = { { { text = note, style = note_style } } } },
        modal.footer({ { "^U", "clear" } }, "Rename"),
      },
    })
  end,

  on_key = function(key)
    local ask = pending()
    if not ask then
      return false
    end
    if ask.waiting then
      return true
    end
    if key.key == "esc" then
      store.rename = nil
      return true
    end
    local field = field_of(ask)
    if key.key == "enter" then
      ask.field, ask.waiting, ask.error = field, true, nil
      store.rename = ask
      command("rename", { session = ask.session, text = field.value })
      return true
    end
    if textinput.key(field, key) then
      ask.field, ask.error = field, nil
      store.rename = ask
    end
    return true
  end,

  on_event = function(name, payload)
    local ask = pending()
    if not ask or not ask.waiting then
      return
    end
    if payload.kind ~= "rename" or payload.session ~= ask.session then
      return
    end
    if name == "command.done" then
      store.rename = nil
    else
      ask.waiting, ask.error = false, payload.error or "rename failed"
      store.rename = ask
    end
  end,

  on_click = function(_)
    return true
  end,
}
