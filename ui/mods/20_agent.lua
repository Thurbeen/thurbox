local chrome = require("lib.chrome")
local panels = require("lib.panels")
local hover = require("lib.hover")
local mod_settings = require("lib.settings")
local session_model = require("lib.session_model")
local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

local NAME = "agent"

local function shell_enabled()
  return mod_settings.feature("shell_pane", true) ~= false
end

local AGENT_TAB, SHELL_TAB = "agent", "shell"
local SELECT_AGENT, SELECT_SHELL = "terminal.agent", "terminal.shell"
local SCROLL_UP, SCROLL_DOWN = "terminal.scroll_up", "terminal.scroll_down"
local SCROLL_LINES = 10
local FOCUS = "terminal.focus"
local REVEAL = "terminal.reveal"
local SURFACE_SCROLL_MAX = 65535

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

local function tab_of(id)
  if not id then
    return AGENT_TAB
  end
  return state["tab:" .. id] or AGENT_TAB
end

local function set_tab(id, tab)
  state["tab:" .. id] = tab ~= AGENT_TAB and tab or nil
end

local function surface_of(id, tab)
  if not id then
    return nil
  end
  return tab == SHELL_TAB and (id .. "#shell") or id
end

local function scroll_of(surface)
  if not surface then
    return 0, 0
  end
  return state["scroll:" .. surface] or 0, state["scrollmax:" .. surface] or 0
end

local function set_scroll(surface, scroll, scroll_max)
  if not surface then
    return
  end
  state["scroll:" .. surface] = scroll ~= 0 and scroll or nil
  state["scrollmax:" .. surface] = scroll_max ~= 0 and scroll_max or nil
  state["mark:" .. surface] = nil
end

local function mark_of(surface)
  return surface and state["mark:" .. surface] or nil
end

local function scroll_surface(surface, lines)
  if not surface then
    return false
  end
  local scroll, scroll_max = scroll_of(surface)
  local moved = math.max(0, scroll + lines)
  if moved == scroll then
    return false
  end
  set_scroll(surface, moved, math.max(scroll_max, moved))
  return true
end

local function scroll_by(id, lines)
  if not id or tab_of(id) ~= AGENT_TAB then
    return false
  end
  scroll_surface(id, lines)
  -- MUST KILL: rename — `scroll_agent_tab_keeping_key_from_pty` would say why the key is claimed even when nothing moved
  return true
end

-- MUST KILL: type — a `RevealRequest` table in `store` instead of the packed `"[-]<surface> <offset> <row>;…"` string would carry its own shape
local function reveal()
  local spec = store[REVEAL]
  if type(spec) ~= "string" then
    return
  end
  for request in spec:gmatch("[^;]+") do
    local reset, surface, offset, row = request:match("^(%-?)(%S+)%s*(%d*)%s*(%d*)$")
    if surface then
      local id = surface:gsub("#shell$", "")
      local shell = id ~= surface
      local _, scroll_max = scroll_of(surface)
      if reset == "-" then
        set_scroll(surface, 0, scroll_max)
        if shell then
          set_tab(id, AGENT_TAB)
        end
      elseif not shell or shell_enabled() then
        local scroll = math.min(tonumber(offset) or 0, SURFACE_SCROLL_MAX)
        set_tab(id, shell and SHELL_TAB or AGENT_TAB)
        set_scroll(surface, scroll, math.max(scroll_max, scroll))
        state["mark:" .. surface] = tonumber(row)
      end
    end
  end
end

local function snap_to_bottom(id)
  local surface = surface_of(id, tab_of(id))
  if not surface then
    return false
  end
  local scroll, scroll_max = scroll_of(surface)
  if scroll == 0 then
    return false
  end
  set_scroll(surface, 0, scroll_max)
  return true
end

local function fit_right_title(title, border_width, reserved_left)
  reserved_left = reserved_left or 0
  if reserved_left == 0 then
    return title
  end
  local available = math.max(0, (border_width or 0) - 2 - reserved_left - 1)
  return widgets.truncate_hard(title, available)
end

local function status_word(session)
  if session.starting then
    return "Starting"
  end
  if session.status == "stopped" and (session.state_why or ""):find("^paused") then
    return "Paused"
  end
  local word = session.status or "idle"
  return (word:gsub("^%l", string.upper))
end

local function agent_word(session)
  local agent = session.agent or ""
  local detected = session.detected_agent
  if detected and detected ~= agent then
    return agent .. " → " .. detected
  end
  return agent
end

local function terminal_title(session, opts)
  opts = opts or {}
  local name = session.name or ""
  local base
  if opts.shell then
    base = " " .. name .. " (shell) "
  elseif session.branch then
    base = " "
      .. name
      .. " ("
      .. agent_word(session)
      .. ") ["
      .. session.branch
      .. "] ["
      .. status_word(session)
      .. "] "
  else
    base = " " .. name .. " (" .. agent_word(session) .. ") [" .. status_word(session) .. "] "
  end

  local scroll = opts.scroll or 0
  if scroll > 0 then
    base = (base:gsub("%s+$", "")) .. " [" .. scroll .. "↑] "
  end
  return base
end

local DRAG = "drag"

local SCROLLBAR = { begin_ = "▲", end_ = "▼", track = "║", thumb = "█" }

local function rounding_divide(numerator, denominator)
  return math.floor((numerator + math.floor(denominator / 2)) / denominator)
end

local function thumb_geometry(track_length, content_len, viewport, position)
  if content_len <= 0 or track_length <= 0 then
    return nil
  end

  local max_position = math.max(0, content_len - 1)
  local start_position = math.max(0, math.min(position, max_position))
  local max_viewport_position = max_position + viewport
  if max_viewport_position == 0 then
    return nil
  end

  local thumb_length = rounding_divide(viewport * track_length, max_viewport_position)
  thumb_length = math.max(1, math.min(thumb_length, track_length))

  local thumb_start = rounding_divide(start_position * track_length, max_viewport_position)
  thumb_start = math.max(0, math.min(thumb_start, track_length - thumb_length))
  return thumb_start, thumb_length
end

local function position_of_thumb(track_length, content_len, viewport, thumb_start)
  local max_position = math.max(0, content_len - 1)
  local _, thumb_length = thumb_geometry(track_length, content_len, viewport, 0)
  if not thumb_length then
    return 0
  end
  local travel = track_length - thumb_length
  if travel <= 0 then
    return max_position
  end
  local at = math.max(0, math.min(thumb_start, travel))
  return math.min(max_position, rounding_divide(at * max_position, travel))
end

local function scrollbar_rows(height, content_len, viewport, position)
  local track_length = height - 2
  local thumb_start, thumb_length = thumb_geometry(track_length, content_len, viewport, position)
  if not thumb_start then
    return nil
  end

  local track_end = track_length - (thumb_start + thumb_length)

  local track_run = { text = SCROLLBAR.track, style = { fg = theme.muted }, role = DRAG }
  local thumb_run = { text = SCROLLBAR.thumb, style = { fg = theme.accent }, role = DRAG }

  local rows = { { text = SCROLLBAR.begin_, role = DRAG } }
  for _ = 1, thumb_start do
    rows[#rows + 1] = track_run
  end
  for _ = 1, thumb_length do
    rows[#rows + 1] = thumb_run
  end
  for _ = 1, track_end do
    rows[#rows + 1] = track_run
  end
  rows[#rows + 1] = { text = SCROLLBAR.end_, role = DRAG }
  return rows
end

local function bar_content_len(depth)
  return depth + 1
end

local function scrollbar_grab(id, hit)
  local surface = surface_of(id, tab_of(id))
  local scroll, depth = scroll_of(surface)
  local height = hit.h or 0
  local track = height - 2
  local content_len = bar_content_len(depth)
  local thumb_start, thumb_length = thumb_geometry(track, content_len, height, depth - scroll)
  if not thumb_start then
    return false
  end

  local row = math.max(0, math.min((hit.y or 0) - 1, track - 1))

  if not hit.dragging then
    local inside = row - thumb_start
    local held = (inside > 0 and inside < thumb_length) and inside or nil
    state["grab:" .. surface] = held
  end

  local position =
    position_of_thumb(track, content_len, height, row - (state["grab:" .. surface] or 0))
  set_scroll(surface, math.max(0, math.min(depth - position, depth)), depth)
  return true
end

local COLLAPSE_CHEVRON_CELLS = 3
local COLLAPSE_TOGGLE_MIN_WIDTH = 5
local COLLAPSE_HINT_MIN_WIDTH = 40

-- MUST KILL: rearchitect — the kernel publishing each binding's compact spelling would retire this mirror of `KeyChord::compact`
local function compact_chord(chord)
  local modifiers, key = "", chord
  while true do
    local prefix, rest = string.match(key, "^(%a+)%+(.*)$")
    if not prefix then
      break
    end
    local symbol = ({ ctrl = "^", shift = "⇧", alt = "⌥", cmd = "⌘" })[prefix]
    if not symbol then
      break
    end
    modifiers = modifiers .. symbol
    key = rest
  end
  if widgets.chars(key) == 1 then
    key = string.upper(key)
  elseif widgets.chars(key) > 1 then
    key = string.upper(string.sub(key, 1, 1)) .. string.sub(key, 2)
  end
  return modifiers .. key
end

local shortcut_cache = { src = nil, by_action = {} }

local function shortcut_for(action)
  local registry = thurbox and thurbox.registry
  local keys = (registry and registry.keys) or {}
  if not rawequal(registry, shortcut_cache.src) then
    shortcut_cache.src = registry
    shortcut_cache.by_action = {}
  end
  local cached = shortcut_cache.by_action[action]
  if cached ~= nil then
    return cached or nil
  end
  local first
  local found
  for _, binding in ipairs(keys) do
    if binding.action == action and binding.key then
      if string.match(binding.key, "^f%d+$") then
        found = compact_chord(binding.key)
        break
      end
      first = first or binding.key
    end
  end
  found = found or (first and compact_chord(first))
  shortcut_cache.by_action[action] = found or false
  return found
end

local function chip_style(primary)
  if primary then
    return { fg = theme.role("selection_fg"), bg = theme.role("selection_bg"), bold = true }
  end
  return { fg = theme.role("text_muted") }
end

local function chip_hover_style()
  return { fg = theme.role("inverted_fg"), bg = theme.role("accent_bright"), bold = true }
end

local function collapse_label(width)
  if width < COLLAPSE_TOGGLE_MIN_WIDTH then
    return nil
  end
  local chevron = panels.shown("sessions") and "◀" or "▶"
  local hint = width >= COLLAPSE_HINT_MIN_WIDTH and shortcut_for("sessions.toggle_panel") or nil
  if hint then
    return " " .. chevron .. " " .. hint .. " "
  end
  return " " .. chevron .. " "
end

local function tab_specs(active)
  local specs = {
    { name = "Agent", active = active == AGENT_TAB, role = "action:" .. SELECT_AGENT },
  }
  if shell_enabled() then
    specs[#specs + 1] = {
      name = "Shell",
      active = active == SHELL_TAB,
      shortcut = shortcut_for("shell.open"),
      role = "action:" .. SELECT_SHELL,
    }
  end
  return specs
end

local function tab_label(spec)
  if spec.shortcut then
    return spec.name .. " · " .. spec.shortcut
  end
  return spec.name
end

local function tabs_block_width(specs)
  if #specs == 0 then
    return 0
  end
  local total = 0
  for _, spec in ipairs(specs) do
    total = total + widgets.len(tab_label(spec)) + 2
  end
  return total + #specs - 1
end

local function trim_tabs(specs, usable)
  while #specs > 1 and tabs_block_width(specs) > usable do
    local stripped = false
    for _, spec in ipairs(specs) do
      if spec.shortcut then
        spec.shortcut = nil
        stripped = true
      end
    end
    if not stripped then
      local victim
      for _, name in ipairs({ "Shell" }) do
        for index, spec in ipairs(specs) do
          if not victim and spec.name == name and not spec.active then
            victim = index
          end
        end
      end
      if not victim then
        return specs
      end
      table.remove(specs, victim)
    end
  end
  return specs
end

local function border_strip(width, border_style, active, rule)
  local runs, cursor = {}, 1
  local function put(at, text, style, role)
    if at > cursor then
      runs[#runs + 1] = { text = string.rep(rule, at - cursor), style = border_style }
    end
    runs[#runs + 1] = { text = text, style = style, role = role }
    cursor = at + widgets.len(text)
  end

  local label = collapse_label(width)
  if label then
    local toggle = "action:sessions.toggle_panel"
    local lit = hover.role(toggle)
    local band = lit and theme.role("selection_bg") or nil
    put(
      1,
      widgets.keep_left(label, COLLAPSE_CHEVRON_CELLS),
      { fg = theme.accent, bg = band },
      toggle
    )
    local hint = widgets.keep_right(label, widgets.len(label) - COLLAPSE_CHEVRON_CELLS)
    if hint ~= "" then
      put(cursor, hint, { fg = theme.muted, bg = band }, toggle)
    end
  end

  local start = label and (cursor + 1) or 1
  local specs = trim_tabs(tab_specs(active), math.max(0, (width - 1) - start))
  local limit = width - 1
  local x = start
  for index, spec in ipairs(specs) do
    local gap = (index > 1) and 1 or 0
    local chip = widgets.len(tab_label(spec)) + 2
    if x + gap + chip > limit then
      break
    end
    x = x + gap
    put(
      x,
      " " .. tab_label(spec) .. " ",
      hover.style(spec.role, chip_hover_style(), chip_style(spec.active)),
      spec.role
    )
    x = x + chip
  end

  return runs, cursor
end

local function border_frame(title, level, border, strip, bar)
  return {
    title = { { text = title, style = chrome.title_style(level) } },
    title_align = "right",
    border_type = chrome.border_type(level),
    border_style = border,
    overlay = { top_left = strip, right_column = bar },
  }
end

local HINT_W, HINT_H = 33, 5

local function hint_box_lines()
  local border = { fg = theme.border }
  local inner = HINT_W - 2

  local function row(runs)
    local width = 0
    for _, run in ipairs(runs) do
      width = width + widgets.len(run.text)
    end
    local line = { { text = chrome.SQUARE.v, style = border } }
    for _, run in ipairs(runs) do
      line[#line + 1] = run
    end
    line[#line + 1] = { text = string.rep(" ", math.max(0, inner - width)) }
    line[#line + 1] = { text = chrome.SQUARE.v, style = border }
    return line
  end

  local rule = string.rep(chrome.SQUARE.h, inner)
  return {
    { { text = chrome.SQUARE.tl .. rule .. chrome.SQUARE.tr, style = border } },
    row({ { text = "No active sessions", style = { fg = theme.secondary } } }),
    row({}),
    row({
      { text = "  F1    ", style = { fg = theme.hint } },
      { text = "  Help", style = { fg = theme.muted } },
    }),
    { { text = chrome.SQUARE.bl .. rule .. chrome.SQUARE.br, style = border } },
  }
end

local function empty_body(inner_w, inner_h)
  if inner_w < HINT_W or inner_h < HINT_H then
    return { type = "text", fill = 1, text = "" }
  end
  return {
    type = "box",
    axis = "vertical",
    fill = 1,
    children = {
      { type = "text", len = math.floor((inner_h - HINT_H) / 2), text = "" },
      {
        type = "box",
        axis = "horizontal",
        len = HINT_H,
        children = {
          { type = "text", len = math.floor((inner_w - HINT_W) / 2), text = "" },
          { type = "text", len = HINT_W, text = hint_box_lines() },
          { type = "text", fill = 1, text = "" },
        },
      },
      { type = "text", fill = 1, text = "" },
    },
  }
end

local function centered(lines)
  local children = { { type = "text", fill = 1, text = "" } }
  for _, line in ipairs(lines) do
    children[#children + 1] = { type = "text", len = 1, align = "center", text = { line } }
  end
  children[#children + 1] = { type = "text", fill = 1, text = "" }
  return { type = "box", axis = "vertical", fill = 1, children = children }
end

local function muted(text)
  return { text = text, style = { fg = theme.muted } }
end

local function key(chord)
  return { text = chord, style = { fg = theme.hint, bold = true } }
end

local function starting_lines(name, elapsed)
  return {
    {
      { text = theme.spinner_frame(elapsed) .. " ", style = { fg = theme.warn } },
      { text = "Starting " .. name .. "…", style = { fg = theme.warn, bold = true } },
    },
    { muted("Fleet is resuming its agent;") },
    { muted("its terminal attaches as soon as it runs.") },
  }
end

local function headline(name, session, revival)
  if revival == "recreate" then
    return name .. " cannot resume"
  end
  local why = session.state_why or ""
  if why:find("^lost") then
    return name .. " is lost"
  end
  if why:find("^paused") then
    return name .. " is paused"
  end
  return name .. " is stopped"
end

local function how_to_start()
  local chord = ui.chord("sessions.start")
  if not chord then
    return { { muted("Choose Start in the session list's menu") }, { muted("to resume it.") } }
  end
  return {
    { muted("Press "), key(chord), muted(" in the session list,") },
    { muted("or choose Start in its menu, to resume it.") },
  }
end

local function how_to_recreate()
  local delete, create = ui.chord("sessions.delete"), ui.chord("new_session.open")
  if not (delete and create) then
    return { { muted("Delete it, then create a new session.") } }
  end
  return {
    { muted("Delete it with "), key(delete), muted(",") },
    { muted("then create a new session with "), key(create), muted(".") },
  }
end

local function not_running_lines(session, revival, elapsed)
  local name = session.name or "this session"
  if session.starting then
    return starting_lines(name, elapsed)
  end
  local paused = revival == "start" and (session.state_why or ""):find("^paused")
  local style = { fg = paused and theme.accent or theme.secondary, bold = true }
  local title = { { text = headline(name, session, revival), style = style } }
  if paused then
    table.insert(title, 1, { text = "❚❚ ", style = style })
  end
  local lines = {
    title,
    { muted(session.state_why or "") },
    {},
  }
  local how
  if revival == "recreate" then
    lines[#lines + 1] = { muted("Fleet cannot resume its conversation.") }
    how = how_to_recreate()
  else
    how = how_to_start()
  end
  for _, line in ipairs(how) do
    lines[#lines + 1] = line
  end
  if session.start_error then
    lines[#lines + 1] = {}
    lines[#lines + 1] = {
      { text = "Start failed: ", style = { fg = theme.bad, bold = true } },
      { text = session.start_error, style = { fg = theme.bad } },
    }
  end
  return lines
end

local function show_tab(id, tab)
  set_tab(id, tab)
  if tab == SHELL_TAB then
    command("shell", { session = id })
  end
  command("focus", { text = NAME })
end

return {
  name = NAME,
  slot = "center",
  slot_mode = "switch",
  pure = true,
  input = "session",
  order = 20,
  focusable = true,

  keys = {
    {
      key = "ctrl+t",
      action = "shell.open",
      desc = "open a shell here",
      scope = "global",
      group = "UI",
    },
    {
      key = "f8",
      action = "shell.open",
      desc = "open a shell here",
      scope = "global",
      group = "UI",
    },
    {
      key = "pageup",
      action = SCROLL_UP,
      desc = "scroll the agent's output back",
      scope = "mod",
      group = "Terminal",
    },
    {
      key = "pagedown",
      action = SCROLL_DOWN,
      desc = "scroll the agent's output forward",
      scope = "mod",
      group = "Terminal",
    },
  },

  render = function(ctx)
    local width, height = ctx.width or 0, ctx.height or 0
    local level = chrome.level(ctx.focused)
    local border = chrome.border_style(level)
    local session = selected()

    if not session then
      local body = empty_body(math.max(0, width - 2), math.max(0, height - 2))
      if ctx.focused then
        body.frame = chrome.frame("No Session", level)
      else
        body.frame = {
          title = { { text = " No Session " } },
          border_type = "square",
          border_style = { fg = theme.muted },
        }
      end
      return body
    end

    local tab = tab_of(session.id)
    local strip, reserved_left = border_strip(width, border, tab, chrome.rule(level))
    local surface = surface_of(session.id, tab)
    local scroll, depth = scroll_of(surface)
    local base = terminal_title(session, { shell = tab == SHELL_TAB, scroll = scroll })
    local title =
      fit_right_title(chrome.label(base:match("^%s*(.-)%s*$"), level), width, reserved_left)

    local revival = session_model.revival(session)
    if session.starting or revival then
      local body = centered(not_running_lines(session, revival, ctx.elapsed))
      body.frame = border_frame(title, level, border, strip)
      return body
    end

    if session.attach_error then
      local body = centered({
        { { text = "no live terminal", style = { fg = theme.bad, bold = true } } },
        { { text = session.attach_error, style = { fg = theme.muted } } },
      })
      body.frame = border_frame(title, level, border, strip)
      return body
    end

    local rows = nil
    if depth > 0 then
      rows = scrollbar_rows(
        math.max(0, height - 2),
        bar_content_len(depth),
        math.max(0, height - 2),
        depth - scroll
      )
    end

    local mark = mark_of(surface)
    if mark and mark >= height - 2 then
      mark = nil
    end

    return {
      type = "surface",
      session = surface,
      scroll = scroll,
      mark = mark,
      fill = 1,
      frame = border_frame(title, level, border, strip, rows),
    }
  end,

  commands = {
    { action = FOCUS, desc = "focus the agent terminal" },
    { action = SELECT_AGENT, desc = "show the agent tab" },
    { action = SELECT_SHELL, desc = "show the shell tab" },
    { action = REVEAL, desc = "scroll back to the last search result" },
  },

  on_scroll = function(wheel)
    local id = store.selected
    return scroll_surface(surface_of(id, tab_of(id)), wheel.up and 1 or -1)
  end,

  on_key = function()
    local id = store.selected
    snap_to_bottom(id)
    local surface = surface_of(id, tab_of(id))
    if surface then
      state["mark:" .. surface] = nil
    end
    return false
  end,

  on_click = function(hit)
    if hit.role ~= DRAG then
      return false
    end
    return scrollbar_grab(store.selected, hit)
  end,

  on_action = function(action)
    local id = store.selected
    if action == SCROLL_UP then
      return scroll_by(id, SCROLL_LINES)
    end
    if action == SCROLL_DOWN then
      return scroll_by(id, -SCROLL_LINES)
    end
    if action == REVEAL then
      reveal()
      return true
    end
    if action == FOCUS then
      command("focus", { text = NAME })
      return true
    end
    if action == "shell.open" then
      if id and shell_enabled() then
        show_tab(id, tab_of(id) == SHELL_TAB and AGENT_TAB or SHELL_TAB)
      end
      return true
    end
    if not id then
      return false
    end
    if action == SELECT_AGENT then
      show_tab(id, AGENT_TAB)
    elseif action == SELECT_SHELL then
      if not shell_enabled() then
        return true
      end
      show_tab(id, SHELL_TAB)
    else
      return false
    end
    return true
  end,
}
