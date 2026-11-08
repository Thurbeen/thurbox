local items = { "alpha", "beta", "gamma" }
return {
  name = "cards",
  slot = "float",
  floats = true,
  events = { "interface.reloaded" },
  on_event = function(_, payload)
    state.reloaded = payload.reason
  end,
  keys = {
    { key = "ctrl+b", action = "cards.open", desc = "Open cards", scope = "global" },
  },
  on_action = function(action)
    if action ~= "cards.open" then
      return false
    end
    state.open = true
    state.item = 1
    state.calls = 0
    state.empty = false
    return true
  end,
  on_focus_cycle = function(direction)
    state.calls = state.calls + 1
    if state.empty then
      return false
    end
    local step = direction == "next" and 1 or -1
    state.item = (state.item - 1 + step) % #items + 1
    return true
  end,
  on_key = function(key)
    if key.key == "esc" then
      state.open = false
    end
    if key.char == "e" then
      state.empty = true
    end
    return true
  end,
  render = function()
    if not state.open then
      return { type = "text", text = "" }
    end
    return {
      type = "text",
      text = "CARDS "
        .. (state.empty and "empty" or items[state.item])
        .. " calls="
        .. state.calls
        .. " behind="
        .. thurbox.focus
        .. " reload="
        .. (state.reloaded or "none"),
      float = { width = 100, height = 100 },
    }
  end,
}
