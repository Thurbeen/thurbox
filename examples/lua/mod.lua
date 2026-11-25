local theme = require("lib.theme")
local widgets = require("lib.widgets")

return {
  name = "example",

  slot = "center",

  focusable = true,

  keys = {
    { key = "r", action = "example.refresh", desc = "count again", group = "Example" },
  },

  settings = {
    { id = "loud", desc = "Say it in capitals", default = false },
  },

  render = function(ctx)
    local seen = state.seen or 0

    local sessions = (thurbox and thurbox.sessions) or {}
    local line = ("%d session(s), rendered %d time(s)"):format(#sessions, seen)
    if require("lib.settings").enabled("example", "loud", false) then
      line = line:upper()
    end

    return {
      type = "box",
      frame = widgets.panel("Example", ctx.focused),
      children = {
        {
          type = "text",
          len = 1,
          text = { { { text = " " .. line, style = { fg = theme.text } } } },
        },
        { type = "text", len = 1, text = "" },
        widgets.hints({ { "r", "count again" } }),
      },
    }
  end,

  on_action = function(action)
    if action == "example.refresh" then
      state.seen = (state.seen or 0) + 1
      return true
    end
    return false
  end,
}
