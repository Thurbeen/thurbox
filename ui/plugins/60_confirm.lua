-- The confirmation, for a change that cannot be undone.
--
-- It knows nothing about sessions. A pane that is about to do something
-- irreversible leaves the question in `store.confirm`:
--
--   store.confirm = {
--     question = "Delete fix-osc52 and its worktree?",
--     lines = { "3 uncommitted files", "2 commits not on origin" },
--     command = "delete",
--     options = { session = id, force = true },
--   }
--
-- and this asks it, then issues that command on `y`/`enter`. So the pane that
-- owns the data decides *what* is at risk and this decides nothing except how the
-- question looks — which is why the session list can itemise a worktree's state
-- without a confirmation dialog knowing what a worktree is.
--
-- v1 has two bespoke modals for this (`ConfirmDelete`, `ConfirmRestore`), each
-- with the risk assessment welded in.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

--- The pending question, if a pane left one.
local function pending()
  local ask = store.confirm
  if type(ask) ~= "table" or not ask.question then
    return nil
  end
  return ask
end

local function clear()
  store.confirm = nil
end

return {
  name = "confirm",
  -- A slot the arrangement never places: this only ever floats.
  slot = "float",
  order = 60,
  floats = true,
  -- This render reads `store.confirm` and the theme and writes nothing, so the
  -- kernel may reuse the tree — and floats render every frame even while
  -- closed, so without this the closed state costs a Lua call per frame
  -- forever. `store.confirm` writes bump the state version, which is part of
  -- the cache key, so opening and answering stay on the very next frame.
  pure = true,
  -- Never a tab stop: it is up only while it has a question, and it takes every
  -- key while it is.
  focusable = false,

  render = function(_)
    local ask = pending()
    if not ask then
      return { type = "text", text = "" }
    end

    local children = {
      {
        type = "text",
        len = 1,
        text = { { { text = " " .. ask.question, style = { fg = theme.text, bold = true } } } },
      },
    }

    -- What is at risk, itemised by whoever asked. A question with nothing listed
    -- is not padded out — the absence says the change is cheap.
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
    children[#children + 1] = {
      type = "box",
      axis = "horizontal",
      len = 1,
      children = {
        {
          type = "text",
          fill = 1,
          text = {
            {
              { text = " y", style = { fg = theme.hint } },
              { text = " confirm  ", style = { fg = theme.muted } },
              { text = "esc", style = { fg = theme.hint } },
              { text = " cancel", style = { fg = theme.muted } },
            },
          },
        },
        -- The pills replay the keys they name, so a click and a keystroke cannot
        -- come to mean different things.
        {
          type = "text",
          len = 11,
          text = { { { text = "[ Confirm ]", style = { fg = theme.bad, bold = true } } } },
          role = "key:y",
        },
        {
          type = "text",
          len = 10,
          text = { { { text = " [ Cancel ]", style = { fg = theme.muted } } } },
          role = "key:esc",
        },
      },
    }

    local rows = 2
    for _, child in ipairs(children) do
      rows = rows + (child.len or 1)
    end
    return {
      -- Danger-bordered, as v1 frames a destructive confirmation. `cols`, not
      -- `width`: the kernel reads `width` as a share of the screen and `cols`
      -- as cells.
      float = { cols = 60, rows = rows },
      type = "box",
      frame = {
        title = " Confirm ",
        borders = "all",
        border_style = { fg = theme.bad },
        style = { bg = theme.role("modal_bg") },
      },
      children = children,
    }
  end,

  on_key = function(key)
    local ask = pending()
    if not ask then
      return false
    end
    if key.key == "esc" or key.key == "n" then
      clear()
      return true
    end
    if key.key == "y" or key.key == "enter" then
      -- Cleared BEFORE the command is issued: the command lands in a later
      -- snapshot, and a question left up meanwhile would invite a second yes.
      local kind, options = ask.command, ask.options
      clear()
      if type(kind) == "string" and kind ~= "" then
        command(kind, type(options) == "table" and options or {})
      end
      return true
    end
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
    return true
  end,

  --- A click anywhere that is not a pill dismisses nothing and confirms nothing;
  --- the pills carry `key:` roles and never reach here. Declared so the float's
  --- own rect does not fall through to the pane beneath it.
  on_click = function(_)
    return widgets ~= nil
  end,
}
