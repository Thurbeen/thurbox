-- Theme roles.
--
-- This file carries NO colour values. It reads the palette the kernel resolved
-- from the built-in presets, your themes.toml and your persisted choice — the
-- same three sources v1 used, so your themes and your current selection carry
-- over untouched.
--
-- That is the whole contract: a plugin names a ROLE, never a colour. Change the
-- active theme and every pane restyles at once, including panes whose author
-- never heard of your theme. A plugin that hardcodes "#5fafff" opts out of all
-- of that, which is why nothing here hands one out.

local theme = {}

local function roles()
  return (thurbox and thurbox.theme and thurbox.theme.roles) or {}
end

--- A role's colour, or nil when the active theme does not define it.
---
--- nil is deliberate: an undefined role must render as "no colour", never as an
--- arbitrary one that looks deliberate.
function theme.role(name)
  return roles()[name]
end

-- Short names for the roles the bundled panes draw with, resolved on access so
-- a theme change is picked up on the next frame — no reload, no plugin edit.
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

--- Glyph and colour for a session status.
---
--- Colour comes from the theme's own status roles, so a theme that recolours
--- "blocked" recolours it here without this file changing.
function theme.status(name)
  local glyphs = {
    working = "◐",
    blocked = "◆",
    done = "●",
    idle = "○",
    error = "✗",
    unreachable = "⊘",
  }
  local role = {
    working = "status_working",
    blocked = "status_blocked",
    done = "status_done",
    idle = "status_idle",
    error = "status_error",
    unreachable = "status_unreachable",
  }
  return {
    glyph = glyphs[name] or glyphs.idle,
    color = roles()[role[name] or "status_idle"],
  }
end

-- The braille spinner the working state animates through.
theme.spinner = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }

function theme.dim(text)
  return { text = text, style = { fg = theme.muted } }
end

function theme.heading(text)
  return { text = text, style = { fg = theme.accent, bold = true } }
end

--- The active theme's identifier, for anything that wants to show it.
function theme.name()
  return (thurbox and thurbox.theme and thurbox.theme.name) or "default"
end

return theme
