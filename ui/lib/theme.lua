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

--- Declared so the shorthands the metatable serves are checkable: `__index`
--- answers nil for a name it does not know, so without this a misspelt one is a
--- run that renders in the terminal's default colour and looks deliberate. The
--- full role vocabulary is `thurbox.Role` in `thurbox.d.lua`; these are the
--- short names, and the two are kept in step by SHORTHAND below.
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
---@param name thurbox.Role
---@return thurbox.Color?
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
  -- An agent holds the pane and has reported nothing. Filled, because
  -- something IS there; not the working spinner, because no process listing
  -- can tell a turn in flight from a prompt waiting for input.
  running = "◍",
  -- The two silences. Dotted, because the content of both is an absence: no
  -- hooks are wired (`uncovered`), or none have fired yet (`unreported`).
  -- Kept visibly apart from `idle`'s green hollow circle, which is a claim
  -- that the agent said it is at rest.
  uncovered = "◌",
  unreported = "◌",
}
local STATUS_ROLES = {
  working = "status_working",
  blocked = "status_blocked",
  done = "status_done",
  idle = "status_idle",
  error = "status_error",
  unreachable = "status_unreachable",
  running = "status_running",
  uncovered = "status_unknown",
  unreported = "status_unknown",
}

--- Glyph and colour for a session status.
---
--- Colour comes from the theme's own status roles, so a theme that recolours
--- "blocked" recolours it here without this file changing.
---
--- The `idle` fallback is for a word this table has no row for — `stopped`,
--- which is at rest by definition. It is deliberately NOT how the three
--- silences are drawn: `running`, `uncovered` and `unreported` each have a row
--- above, because falling through to `idle` is what made a working
--- driver-launched agent draw the green "the agent says it is at rest" dot.
---@param name thurbox.Status
---@return { glyph: string, color: thurbox.Color? }
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
---@param elapsed number?
---@return string
function theme.spinner_frame(elapsed)
  local frame = math.floor((elapsed or 0) * 8) % #theme.spinner + 1
  return theme.spinner[frame]
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

--- The active theme's identifier, for anything that wants to show it.
---@return string
function theme.name()
  return (thurbox and thurbox.theme and thurbox.theme.name) or "default"
end

return theme
