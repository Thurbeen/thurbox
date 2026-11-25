local settings = require("lib.settings")

local NAME = "attend"

local GRACE_MS = 5000

local function enabled()
  return settings.enabled(NAME, "enabled", true)
end

return {
  name = NAME,
  floats = true,
  events = { "session.status", "focus.session" },
  settings = {
    { id = "enabled", desc = "select a session when it needs you", default = true },
  },
  commands = {
    { action = "attend.toggle", desc = "toggle selecting a session when it needs you" },
  },

  on_event = function(name, payload)
    local now = (thurbox and thurbox.taken_at_ms) or 0
    if name == "focus.session" then
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
