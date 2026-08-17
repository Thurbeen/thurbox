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

return hover
