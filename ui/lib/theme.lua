---@class thurbox.ThemeLib
---@field accent thurbox.Color?
---@field accent_bright thurbox.Color?
---@field muted thurbox.Color?
---@field text thurbox.Color?
---@field secondary thurbox.Color?
---@field ok thurbox.Color?
---@field warn thurbox.Color?
---@field bad thurbox.Color?
---@field info thurbox.Color?
---@field border thurbox.Color?
---@field border_focused thurbox.Color?
---@field branch thurbox.Color?
---@field hint thurbox.Color?
local theme = {}

local cached_theme, cached_roles
local function roles()
  local src = thurbox and thurbox.theme
  if src == nil then
    return {}
  end
  if not rawequal(src, cached_theme) then
    cached_theme = src
    cached_roles = src.roles or {}
  end
  return cached_roles
end

---@param name thurbox.Role
---@return thurbox.Color?
function theme.role(name)
  return roles()[name]
end

local SHORTHAND = {
  accent = "accent",
  accent_bright = "accent_bright",
  muted = "text_muted",
  text = "text_primary",
  secondary = "text_secondary",
  ok = "status_idle",
  warn = "status_working",
  bad = "danger",
  info = "status_done",
  border = "border_unfocused",
  border_focused = "border_focused",
  branch = "branch_name",
  hint = "keybind_hint",
}

setmetatable(theme, {
  __index = function(_, key)
    local role = SHORTHAND[key]
    if role then
      return roles()[role]
    end
    return nil
  end,
})

local STATUS_GLYPHS = {
  working = "◐",
  blocked = "◆",
  done = "●",
  background = "◎",
  idle = "○",
  error = "✗",
  unreachable = "⊘",
  -- Not U+25CD: monospace fonts often lack it and draw a fallback box.
  running = "◉",
  uncovered = "◌",
  unreported = "◌",
  failed = "✗",
}
local STATUS_ROLES = {
  working = "status_working",
  blocked = "status_blocked",
  done = "status_done",
  background = "status_background",
  idle = "status_idle",
  error = "status_error",
  unreachable = "status_unreachable",
  running = "status_running",
  uncovered = "status_unknown",
  unreported = "status_unknown",
  failed = "status_error",
}

---@param name thurbox.Status
---@return { glyph: string, color: thurbox.Color? }
function theme.status(name)
  return {
    glyph = STATUS_GLYPHS[name] or STATUS_GLYPHS.idle,
    color = roles()[STATUS_ROLES[name] or "status_idle"],
  }
end

theme.spinner = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }

local KERNEL_ANIMATION_HZ = 8

---@param elapsed number?
---@return string
function theme.spinner_frame(elapsed)
  local frame = math.floor((elapsed or 0) * KERNEL_ANIMATION_HZ) % #theme.spinner + 1
  return theme.spinner[frame]
end

local function rgb(colour)
  if type(colour) ~= "string" then
    return nil
  end
  local r, g, b = colour:match("^#(%x%x)(%x%x)(%x%x)$")
  if not r then
    return nil
  end
  return { tonumber(r, 16), tonumber(g, 16), tonumber(b, 16) }
end

---@param colour thurbox.Color?
---@param amount number
---@return thurbox.Color?
function theme.tint(colour, amount)
  local over, under = rgb(colour), rgb(roles().app_bg)
  if not over or not under then
    return nil
  end
  local mixed = {}
  for i = 1, 3 do
    mixed[i] = math.floor(under[i] + (over[i] - under[i]) * amount + 0.5)
  end
  return string.format("#%02x%02x%02x", mixed[1], mixed[2], mixed[3])
end

---@param text string
---@return thurbox.Span
function theme.dim(text)
  return { text = text, style = { fg = theme.muted } }
end

---@param text string
---@return thurbox.Span
function theme.heading(text)
  return { text = text, style = { fg = theme.accent, bold = true } }
end

---@return string
function theme.name()
  return (thurbox and thurbox.theme and thurbox.theme.name) or "default"
end

return theme
