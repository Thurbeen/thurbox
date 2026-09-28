-- A menu of actions, opened at a point: the context menu's float.
--
-- It knows nothing about what the entries do. A pane that wants a menu leaves
-- it in `store.menu`:
--
--   store.menu = {
--     at = { x = hit.screen_x, y = hit.screen_y },
--     items = { { label = "Rename", action = "sessions.rename" }, "sep", ... },
--   }
--
-- and this draws it at that point and, on a choice, closes and runs the
-- entry's action through `command("action")` -- the palette's road, so an entry
-- does exactly what its chord does, confirmation included. The action must be
-- declared by some plugin (`keys` or `commands`); an undeclared one falls back to
-- this float, which has no `on_action`, and does nothing. Closed BEFORE the
-- action runs, so an action that opens a float of its own (rename, fork,
-- confirm) is never drawn under a menu that is still up.

local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

--- The open menu, if a pane left one.
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

--- The highlighted entry: the one a key moved to, else the first choosable.
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

--- The next choosable entry from `from` in direction `dir`; `from` at an end.
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
    command("action", { text = item.action })
  end
end

return {
  name = "menu",
  -- A slot the arrangement never places: this only ever floats.
  slot = "float",
  order = 64,
  floats = true,
  -- Reads `store.menu`, the registry and the theme, and writes nothing; a
  -- `store` write bumps the state version the cached tree is keyed on.
  pure = true,
  focusable = false,

  render = function(_)
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
    -- One column of padding each side, and three between a label and its chord.
    local inner = 1 + label_w + (chord_w > 0 and 3 + chord_w or 0) + 1

    local children = {}
    for i, item in ipairs(menu.items) do
      if choosable(item) then
        local label = item.label or item.action
        local gap = inner - 2 - widgets.len(label) - widgets.len(chords[i])
        local style = { fg = theme.text }
        local hint = { fg = theme.hint }
        if i == at then
          style = {
            bg = theme.role("selection_bg"),
            fg = theme.role("selection_fg"),
            bold = true,
          }
          hint = style
        end
        children[#children + 1] = {
          type = "text",
          len = 1,
          id = "menu-" .. i,
          role = "row",
          text = {
            {
              { text = " " .. label .. string.rep(" ", gap), style = style },
              { text = chords[i] .. " ", style = hint },
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
      float = { at = { x = menu.at.x, y = menu.at.y }, cols = inner + 2, rows = #menu.items + 2 },
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
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
    return true
  end,

  --- An entry runs; the rule and the frame are the menu's and do nothing.
  on_click = function(hit)
    local menu = pending()
    local i = hit.id and tonumber(hit.id:match("^menu%-(%d+)$"))
    if menu and i then
      run(menu, i)
    end
    return true
  end,

  -- A press anywhere else closes it -- what every menu does. The kernel has
  -- already swallowed that press, so it selects or focuses nothing beneath.
  on_outside = function(_)
    if pending() then
      store.menu = nil
    end
    return true
  end,
}
