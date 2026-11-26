local theme = require("lib.theme")
local widgets = require("lib.widgets")

local chrome = {}

function chrome.spans_len(spans)
  local total = 0
  for _, span in ipairs(spans) do
    total = total + widgets.len(span.text or "")
  end
  return total
end

chrome.MARK = "▸"

---@param focused boolean?
---@return "focused"|"inactive"
function chrome.level(focused)
  return focused and "focused" or "inactive"
end

function chrome.border_type(level)
  return level == "focused" and "thick" or "rounded"
end

function chrome.rule(level)
  return level == "focused" and "━" or "─"
end

function chrome.border_style(level)
  if level == "focused" then
    return { fg = theme.border_focused, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

function chrome.title_style(level)
  if level == "focused" then
    return { fg = theme.role("inverted_fg"), bg = theme.border_focused, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.secondary }
end

---@param text string
---@param level string
---@return string
function chrome.label(text, level)
  if level == "focused" then
    return " " .. chrome.MARK .. " " .. text .. " "
  end
  return " " .. text .. " "
end

---@param title string
---@param level string
---@return thurbox.Frame
function chrome.frame(title, level)
  return {
    title = { { text = chrome.label(title, level), style = chrome.title_style(level) } },
    border_type = chrome.border_type(level),
    border_style = chrome.border_style(level),
  }
end

chrome.SQUARE = { tl = "┌", tr = "┐", bl = "└", br = "┘", h = "─", v = "│" }

return chrome
