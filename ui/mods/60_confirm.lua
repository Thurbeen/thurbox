local modal = require("lib.modal")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local CONFIRMABLE_COMMANDS = { delete = true, pause = true, restart = true }

local function session_name(id)
  for _, row in ipairs(thurbox.sessions or {}) do
    if row.id == id then
      return row.name or id
    end
  end
  return id or "a session"
end

local function consequence(command, options)
  if command == "pause" then
    return "Pause " .. session_name(options.session)
  end
  if command == "restart" and options.fresh then
    return "Restart " .. session_name(options.session) .. " on a new conversation"
  end
  if command == "restart" then
    return "Restart " .. session_name(options.session)
  end
  return "Delete " .. session_name(options.session) .. " for good"
end

local request_on_screen = nil

local function pending()
  local ask = store.confirm
  if type(ask) ~= "table" or not ask.question or not CONFIRMABLE_COMMANDS[ask.command] then
    return nil
  end
  return ask
end

local function clear()
  store.confirm = nil
  request_on_screen = nil
end

return {
  name = "confirm",
  capabilities = { "sessions" },
  ui_state = function()
    return { open = pending() ~= nil }
  end,
  slot = "float",
  order = 60,
  floats = true,
  pure = true,
  focusable = false,

  render = function(_)
    local ask = pending()
    request_on_screen = ask
    if not ask then
      return { type = "text", text = "" }
    end

    local options = type(ask.options) == "table" and ask.options or {}
    local children = {
      {
        type = "text",
        len = 1,
        text = {
          {
            {
              text = " " .. consequence(ask.command, options) .. "?",
              style = { fg = theme.text, bold = true },
            },
          },
        },
      },
      {
        type = "text",
        len = 1,
        text = {
          {
            {
              text = " asked by " .. (store_writer("confirm") or "thurbox"),
              style = { fg = theme.muted },
            },
          },
        },
      },
    }

    local lines = ask.lines
    if type(lines) == "table" and #lines > 0 then
      children[#children + 1] = { type = "text", len = 1, text = "" }
      for _, line in ipairs(lines) do
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { { { text = "   • " .. line, style = { fg = theme.bad } } } },
        }
      end
    end

    children[#children + 1] = { type = "text", len = 1, text = "" }
    children[#children + 1] = modal.footer(
      { { " y", "confirm" }, { "esc", "cancel" } },
      "Confirm",
      { key = "y", style = { fg = theme.bad, bold = true } }
    )

    local rows = 2
    for _, child in ipairs(children) do
      rows = rows + (child.len or 1)
    end
    return modal.frame("Confirm", {
      cols = 60,
      rows = rows,
      border = { fg = theme.bad },
      children = children,
    })
  end,

  on_key = function(key)
    local ask = pending() and request_on_screen
    if not ask then
      return false
    end
    if key.key == "esc" or key.key == "n" then
      clear()
      return true
    end
    if key.key == "y" or key.key == "enter" then
      local kind, options = ask.command, ask.options
      -- MUST KILL: extract — a `take_answered_request` that clears and returns the request would stop a reorder inviting a second yes
      clear()
      command(kind, type(options) == "table" and options or {})
      return true
    end
    return true
  end,

  on_click = function(_)
    return widgets ~= nil
  end,
}
