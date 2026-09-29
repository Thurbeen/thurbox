-- What the pointer is over.
--
-- The kernel resolves the pointer against the same hitboxes it routes clicks
-- through, and publishes the identity it landed on. Matching here is therefore
-- on the very `role` or `id` the node was given, which is what stops a
-- highlight and a click ever disagreeing about which button is which — the two
-- cannot drift, because they are answered from one registry.
--
-- v1 does the equivalent with `App::mouse_hover`, re-resolving a stored
-- position every frame. Publishing the resolved identity instead means a
-- pointer moving WITHIN one affordance changes nothing, so crossing a pane
-- costs one repaint per affordance rather than one per cell.

local theme = require("lib.theme")

local hover = {}

local function current()
  return (thurbox and thurbox.hover) or {}
end

--- Is the pointer over the node that carries this `role`?
---
--- Nil-safe on purpose: an affordance with no role is not hoverable, and asking
--- should be false rather than an error, so a caller can pass a role that is
--- only sometimes present.
function hover.role(role)
  return role ~= nil and current().role == role
end

--- Is the pointer over the node that carries this `id`?
function hover.id(id)
  return id ~= nil and current().id == id
end

--- Pick between two styles on hover, which is the whole of what most callers
--- want and keeps the conditional out of their span tables.
function hover.style(role, lit, base)
  if hover.role(role) then
    return lit
  end
  return base
end

--- The band a list row wears under the pointer: the background only, so a
--- status dot or a match highlight keeps its colour. The selection bar is the
--- stronger fill, which is why a caller skips this on the selected row.
function hover.row_style()
  return { bg = theme.role("selection_bg") }
end

--- A button under the pointer: `accent_bright` rather than `accent`, so a
--- primary pill already filled with the accent still visibly answers, and
--- `inverted_fg` so a secondary pill stays legible once its fill is replaced.
--- The kernel's own pills and chips use the same pair.
function hover.button_style()
  return { fg = theme.role("inverted_fg"), bg = theme.role("accent_bright"), bold = true }
end

--- The border a field wears: focused outranks hovered, which outranks resting.
--- Hover is the primary text colour rather than the accent, so a lit border is
--- never mistaken for the focused one on a palette whose focus colour IS the
--- accent.
function hover.border(focused, id)
  if focused then
    return theme.border_focused
  end
  if hover.id(id) then
    return theme.text
  end
  return theme.border
end

return hover
