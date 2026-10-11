-- A pane that asks for key releases on one chord and not on another, and
-- reports everything it was handed through `ui_state`.
local log = {}
local plain = 0
local spaces = 0

return {
  name = "hold",
  slot = "hold",
  strip = true,
  size = { len = 1 },
  focusable = false,
  keys = {
    { key = "ctrl+space", action = "hold.talk", desc = "talk while held", scope = "global", release = true },
    { key = "f7", action = "hold.plain", desc = "a press-only chord", scope = "global" },
    { key = "space", action = "hold.space", desc = "where a stray space lands", scope = "global" },
  },
  on_action = function(action, args)
    if action == "hold.talk" then
      log[#log + 1] = args and args.event or "none"
      return true
    end
    if action == "hold.plain" then
      plain = plain + 1
      if args ~= nil then
        log[#log + 1] = "plain-had-args"
      end
      return true
    end
    if action == "hold.space" then
      spaces = spaces + 1
      return true
    end
    return false
  end,
  render = function()
    return { type = "text", text = "HOLD " .. table.concat(log, ",") }
  end,
  ui_state = function()
    return {
      events = table.concat(log, ","),
      plain = plain,
      spaces = spaces,
      releases = thurbox.keyboard and thurbox.keyboard.releases or "absent",
    }
  end,
}
