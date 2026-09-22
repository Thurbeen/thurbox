-- The companion shell as a pane of its own: the selected session's shell, in
-- the `shell` slot.
--
-- The agent pane already shows this shell as its Shell tab. This file exists
-- for arrangements that want both on screen at once — the `split-shell` and
-- `ide` layouts put it below the agent — and it is the same `<id>#shell` surface over the
-- same live terminal, so a keystroke typed here reaches the same shell the tab
-- would have shown.
--
-- `optional = true` because most arrangements leave it out: `classic` does, and
-- so does every layout.lua written before this pane existed. An optional pane
-- whose slot is not placed loads, draws nothing and is not reported by
-- `plugin check`. While it IS on screen the agent pane drops its Shell tab (it
-- asks `lib.panels.placed("shell")`), because one terminal cannot be drawn at
-- two sizes.
--
-- Nothing here asks for the shell to be opened: painting a `<id>#shell` surface
-- for a session that has none is what opens it, so this render writes nothing
-- and the pane can be `pure`.

local chrome = require("lib.chrome")
local plugin_settings = require("lib.settings")
local theme = require("lib.theme")

local NAME = "shell"

--- The session the list published, resolved against the current snapshot.
local function selected()
  local id = store.selected
  if not id then
    return nil
  end
  for _, session in ipairs(thurbox and thurbox.sessions or {}) do
    if session.id == id then
      return session
    end
  end
  return nil
end

local function shell_enabled()
  return plugin_settings.feature("shell_pane", true) ~= false
end

--- Whether this session has a live terminal to open a shell beside.
local function can_open(session)
  return not session.attach_error
    and session.status ~= "stopped"
    and session.status ~= "unreachable"
end

--- How far back the shell's scrollback is showing. Wheel only: the page keys
--- belong to whatever runs in the shell, as they do on the agent pane's tab.
local function scroll_of(surface)
  return state["scroll:" .. surface] or 0
end

local function set_scroll(surface, scroll)
  state["scroll:" .. surface] = scroll ~= 0 and scroll or nil
end

local function frame(title, level)
  return {
    title = { { text = title, style = chrome.title_style(level) } },
    title_align = "right",
    border_style = chrome.border_style(level),
  }
end

--- One run of text, centred in the pane.
local function centered(run)
  return {
    type = "box",
    axis = "vertical",
    fill = 1,
    children = {
      { type = "text", fill = 1, text = "" },
      { type = "text", len = 1, align = "center", text = { { run } } },
      { type = "text", fill = 1, text = "" },
    },
  }
end

return {
  name = NAME,
  slot = "shell",
  optional = true,
  pure = true,
  input = "session",
  order = 25,
  focusable = true,

  render = function(ctx)
    local level = ctx.focused and "focused" or "active"
    -- Whether the keyboard is here, for the agent pane: when an arrangement
    -- takes this pane off screen while it has focus, the shell goes on in the
    -- agent pane's Shell tab rather than handing the next keystroke to the
    -- agent. Only while drawn, so the last frame this pane painted is what
    -- answers. Written on change, like every other shared value.
    if (store["shell.focused"] == true) ~= ctx.focused then
      store["shell.focused"] = ctx.focused or nil
    end
    local session = selected()

    if not session then
      local body = centered({ text = "no session selected", style = { fg = theme.muted } })
      body.frame = frame(" Shell ", level)
      return body
    end

    local title = " " .. (session.name or "") .. " (shell) "
    if not shell_enabled() then
      local body = centered({
        text = "the shell is off: features.shell_pane in settings",
        style = { fg = theme.muted },
      })
      body.frame = frame(title, level)
      return body
    end
    if not can_open(session) then
      local body = centered({ text = "no live terminal", style = { fg = theme.muted } })
      body.frame = frame(title, level)
      return body
    end

    local surface = session.id .. "#shell"
    local scroll = scroll_of(surface)
    if scroll > 0 then
      title = (title:gsub("%s+$", "")) .. " [" .. scroll .. "↑] "
    end
    return {
      type = "surface",
      session = surface,
      scroll = scroll,
      fill = 1,
      frame = frame(title, level),
    }
  end,

  on_scroll = function(wheel)
    local id = store.selected
    if not id then
      return false
    end
    local surface = id .. "#shell"
    local scroll = scroll_of(surface)
    local moved = math.max(0, scroll + (wheel.up and 1 or -1))
    if moved == scroll then
      return false
    end
    set_scroll(surface, moved)
    return true
  end,

  -- You type at the end of what you are typing into, so a keystroke brings a
  -- scrolled-back shell to its live bottom before it reaches the pty.
  on_key = function()
    local id = store.selected
    if id then
      set_scroll(id .. "#shell", 0)
    end
    return false
  end,
}
