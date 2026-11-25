local theme = require("lib.theme")

local hover = {}

local function current()
  return (thurbox and thurbox.hover) or {}
end

function hover.role(role)
  return role ~= nil and current().role == role
end

function hover.id(id)
  return id ~= nil and current().id == id
end

function hover.style(role, lit, base)
  if hover.role(role) then
    return lit
  end
  return base
end

function hover.row_style()
  return { bg = theme.role("selection_bg") }
end

function hover.button_style()
  return { fg = theme.role("inverted_fg"), bg = theme.role("accent_bright"), bold = true }
end

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
