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

-- Resolved on access so a theme change is picked up on the next frame, and
-- memoized on the published table's identity: `thurbox.theme` is a gated
-- group, so seeing the same table object again means the same roles. Without
-- the memo every `theme.muted`-style access walks thurbox → theme → roles —
-- dozens of crossings per rendered row.
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

-- Hoisted: `theme.status` runs once per rendered row (and again per border
-- dot), and building these two tables per call was three allocations each time.
local STATUS_GLYPHS = {
  working = "◐",
  blocked = "◆",
  done = "●",
  idle = "○",
  error = "✗",
  unreachable = "⊘",
}
local STATUS_ROLES = {
  working = "status_working",
  blocked = "status_blocked",
  done = "status_done",
  idle = "status_idle",
  error = "status_error",
  unreachable = "status_unreachable",
}

--- Glyph and colour for a session status.
---
--- Colour comes from the theme's own status roles, so a theme that recolours
--- "blocked" recolours it here without this file changing.
function theme.status(name)
  return {
    glyph = STATUS_GLYPHS[name] or STATUS_GLYPHS.idle,
    color = roles()[STATUS_ROLES[name] or "status_idle"],
  }
end

-- The braille spinner the working state animates through.
theme.spinner = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }

--- The spinner glyph for this frame.
---
--- Here rather than in `lib/widgets.lua` so that it ships in the same file as
--- the table it indexes. A pane and a `lib/` module are delivered separately and
--- an edit to either is preserved, so a directory can carry an updated pane
--- beside a module predating a helper it calls; the call then throws from
--- `render`, and a pane that throws records no click targets, so its rows stop
--- answering the mouse. Nothing makes a pane immune to a stale module — but the
--- glyphs and the arithmetic over them can at least not skew against each
--- other.
---
--- The 8 is the kernel's `ANIMATION_HZ` (`kernel::host`), and the two must stay
--- in lockstep: the shared animation clock invalidates `pure` panes at that rate
--- and only while something is animating, so a spinner advancing faster than the
--- clock would skip frames and one advancing slower would be re-rendered for no
--- visible change.
function theme.spinner_frame(elapsed)
  local frame = math.floor((elapsed or 0) * 8) % #theme.spinner + 1
  return theme.spinner[frame]
end

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
