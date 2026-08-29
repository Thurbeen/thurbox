-- Bring a session forward the moment it needs you.
--
-- The first example that REACTS rather than draws. It occupies no slot and
-- paints nothing; it subscribes to two events — `session.status`, to notice a
-- session going `blocked`, and `focus.session`, to notice you moving — and when
-- a session blocks while you have not moved the selection for a few seconds,
-- that session becomes the selected one, exactly as `thurbox-cli session focus`
-- would select it.
--
-- Two things worth copying. It never steals focus from a person: a selection
-- moved less than `GRACE_MS` ago is a person's, and the event is let go. And it
-- can be switched off without a reload — a palette command (`Ctrl+P`, then
-- "attend") flips the declared setting, which the settings modal then shows
-- correctly, because the value has exactly one home.
--
-- To use it, copy it into your interface directory (`thurbox-cli plugin dir`).
-- It needs no trust: it reads the snapshot and issues commands, like every
-- other plugin. `thurbox-cli plugin events` lists what else it could listen for.

local settings = require("lib.settings")

local NAME = "attend"

--- How long after you moved the selection a blocked session waits its turn.
local GRACE_MS = 5000

local function enabled()
  return settings.enabled(NAME, "enabled", true)
end

return {
  name = NAME,
  -- Declared floating and never returning a float node: a plugin that draws
  -- nothing still has to say where it would draw, and a float occupies no slot —
  -- so `plugin check` does not ask the arrangement to place it.
  floats = true,
  events = { "session.status", "focus.session" },
  settings = {
    { id = "enabled", desc = "select a session when it needs you", default = true },
  },
  commands = {
    { action = "attend.toggle", desc = "toggle selecting a session when it needs you" },
  },

  on_event = function(name, payload)
    -- A plugin has no clock; the snapshot's own instant is the nearest thing,
    -- and it is current to within one refresh.
    local now = (thurbox and thurbox.taken_at_ms) or 0
    if name == "focus.session" then
      -- The selection this plugin just asked for arrives back as an event too;
      -- only a move it did not make counts as the person's.
      if state.asked == payload.to then
        state.asked = nil
      else
        state.moved_at = now
      end
      return
    end
    if payload.to ~= "blocked" or not enabled() then
      return
    end
    if state.moved_at and now - state.moved_at < GRACE_MS then
      return
    end
    state.asked = payload.session
    -- The same request a clicked notification leaves: the session list owns the
    -- selection and consumes this on its next frame.
    store.focus_session = payload.session
  end,

  on_action = function(action)
    if action ~= "attend.toggle" then
      return false
    end
    command("set", { text = NAME .. ".enabled", flag = not enabled() })
    return true
  end,

  render = function()
    return { type = "text", text = "" }
  end,
}
