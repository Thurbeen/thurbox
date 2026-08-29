-- A starting point. Rename it, change what it draws, keep what it shows you.
--
-- This is what `thurbox-cli plugin new <name>` writes, and what the guide shows,
-- so what you read is what you get. Everything below is the shape of a pane:
-- declare what you are, return what to draw, handle what you declared.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

return {
  -- Your pane's identity. Actions are namespaced with it (`example.refresh`),
  -- and it is what `focus:` roles and the settings modal group by.
  name = "example",

  -- Where you draw. `center` shares the middle with the agent (one visible at a
  -- time); `sessions` is the left column. A slot `ui/layout.lua` does not place
  -- is never drawn — which is how a float declares that it takes no room.
  slot = "center",

  -- Focusable panes join the Tab ring and can hold the cursor.
  focusable = true,

  -- Keys are DATA, not just handled: declaring them is what puts them in the F1
  -- help, lets a user rebind them, and lets the kernel catch a clash with
  -- another plugin. `scope = "global"` fires from any pane.
  keys = {
    { key = "r", action = "example.refresh", desc = "count again", group = "Example" },
  },

  -- Settings are data too: declare one and the F6 modal grows a row for it,
  -- without knowing what it means. Read it back through `lib.settings`.
  settings = {
    { id = "loud", desc = "Say it in capitals", default = false },
  },

  -- Called with YOUR pane's resolved size — not the screen's — which is what
  -- makes wrapping, truncation and scroll windows possible at all.
  render = function(ctx)
    -- `state` is yours and survives a reload. Reading it hands back a COPY, so
    -- mutate the copy and write the whole thing back; a nested field changed in
    -- place is silently lost. This is the trap that catches everyone once.
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

  -- Declared keys arrive here as actions, already resolved — so a rebind costs
  -- you nothing. Return true when you consumed it.
  on_action = function(action)
    if action == "example.refresh" then
      state.seen = (state.seen or 0) + 1
      return true
    end
    return false
  end,
}
