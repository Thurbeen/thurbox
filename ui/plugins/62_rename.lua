-- Renaming a session: one field, holding the name it has now.
--
-- It knows nothing about sessions beyond an id and a name. A pane that wants a
-- session renamed leaves the request in `store.rename`:
--
--   store.rename = { session = id, name = "fix-osc52" }
--
-- and this owns the field, issues `rename` on enter, and stays up until the
-- command answers — so a name the kernel refuses is explained here, beside the
-- text that caused it, rather than as a failure on a row the eye has left. The
-- rules are the kernel's alone (`session_ops::rename`); a copy of them in Lua
-- would be a second list to drift.

local modal = require("lib.modal")
local textinput = require("lib.textinput")
local theme = require("lib.theme")

--- The open request, if a pane left one.
local function pending()
  local ask = store.rename
  if type(ask) ~= "table" or type(ask.session) ~= "string" then
    return nil
  end
  return ask
end

--- The field, built from the current name the first time it is needed. Render
--- must not write, so a handler that edits the field stores what this returns.
local function field_of(ask)
  return ask.field or textinput.new(ask.name)
end

return {
  name = "rename",
  -- A slot the arrangement never places: this only ever floats.
  slot = "float",
  order = 62,
  floats = true,
  -- Reads `store.rename` and the theme and writes nothing; a `store` write bumps
  -- the state version the cached tree is keyed on.
  pure = true,
  focusable = false,
  -- The answer: a command is accepted at once and finishes later.
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
      -- The border, the framed field, the note and the footer.
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
    -- Nothing happens while the answer is out, Esc included: the rename still
    -- lands after the float is gone, so closing would look like a cancel that
    -- was not one, drop the reason for a refusal, and let that late answer
    -- close a second request for the same session. The field would also stop
    -- being the name that was sent.
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
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
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

  --- Declared so a click on the float's own rect does not fall through to the
  --- pane beneath it; the pills carry `key:` roles and never reach here.
  on_click = function(_)
    return true
  end,
}
