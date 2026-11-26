local hover = require("lib.hover")
local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

local function pending()
  local menu = store.menu
  if type(menu) ~= "table" or type(menu.items) ~= "table" or type(menu.at) ~= "table" then
    return nil
  end
  return menu
end

local function choosable(item)
  return type(item) == "table" and type(item.action) == "string"
end

local function current(menu)
  if menu.index and choosable(menu.items[menu.index]) then
    return menu.index
  end
  for i, item in ipairs(menu.items) do
    if choosable(item) then
      return i
    end
  end
  return nil
end

local function step(menu, from, dir)
  local i = from + dir
  while menu.items[i] ~= nil do
    if choosable(menu.items[i]) then
      return i
    end
    i = i + dir
  end
  return from
end

local function run(menu, i)
  local item = menu.items[i]
  store.menu = nil
  if choosable(item) then
    store["menu.chosen"] = { action = item.action, target = menu.target }
    if menu.target_argument == "session_id" then
      command("action", { text = item.action, session = menu.target })
    else
      command("action", { text = item.action, target = menu.target })
    end
  end
end

local function first_shown(at, shown)
  if not at or at <= shown then
    return 1
  end
  return at - shown + 1
end

return {
  name = "menu",
  ui_state = function()
    local menu = pending()
    if not menu then
      return { open = false }
    end
    return { open = true, selection = current(menu) or 0 }
  end,
  slot = "float",
  order = 64,
  floats = true,
  pure = true,
  focusable = false,

  render = function(ctx)
    local menu = pending()
    if not menu then
      return { type = "text", text = "" }
    end
    local at = current(menu)
    local chords, label_w, chord_w = {}, 0, 0
    for i, item in ipairs(menu.items) do
      if choosable(item) then
        chords[i] = ui.chord(item.action) or ""
        label_w = math.max(label_w, widgets.len(item.label or item.action))
        chord_w = math.max(chord_w, widgets.len(chords[i]))
      end
    end
    local inner = 1 + label_w + (chord_w > 0 and 3 + chord_w or 0) + 1

    local shown = math.max(1, math.min(#menu.items, ctx.height - 2))
    local first = first_shown(at, shown)

    local children = {}
    for i = first, first + shown - 1 do
      local item = menu.items[i]
      if choosable(item) then
        local label = item.label or item.action
        local gap = inner - 2 - widgets.len(label) - widgets.len(chords[i])
        local id = "menu-" .. i
        local style = { fg = theme.text }
        if i == at then
          style = {
            bg = theme.role("selection_bg"),
            fg = theme.role("selection_fg"),
            bold = true,
          }
        elseif hover.id(id) then
          style = { fg = theme.text, bg = hover.row_style().bg }
        end
        children[#children + 1] = {
          type = "text",
          len = 1,
          id = id,
          role = "row",
          style = style,
          text = {
            {
              { text = " " .. label .. string.rep(" ", gap) },
              { text = chords[i] .. " ", style = { fg = theme.hint } },
            },
          },
        }
      else
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { { { text = string.rep("─", inner), style = { fg = theme.muted } } } },
        }
      end
    end

    return {
      float = { at = { x = menu.at.x, y = menu.at.y }, cols = inner + 2, rows = shown + 2 },
      type = "box",
      frame = {
        borders = "all",
        border_style = { fg = theme.role("modal_border") },
        style = { bg = theme.role("modal_bg") },
      },
      children = children,
    }
  end,

  on_key = function(key)
    local menu = pending()
    if not menu then
      return false
    end
    local at = current(menu)
    local name = key.key
    if name == "esc" then
      store.menu = nil
    elseif name == "enter" then
      if at then
        run(menu, at)
      end
    elseif at and (name == "down" or (name == "j" and not key.ctrl)) then
      menu.index = step(menu, at, 1)
      store.menu = menu
    elseif at and (name == "up" or (name == "k" and not key.ctrl)) then
      menu.index = step(menu, at, -1)
      store.menu = menu
    end
    return true
  end,

  on_click = function(hit)
    local menu = pending()
    local i = hit.id and tonumber(hit.id:match("^menu%-(%d+)$"))
    if menu and i then
      run(menu, i)
    end
    return true
  end,

  on_outside = function(_)
    if pending() then
      store.menu = nil
    end
    return true
  end,
}
