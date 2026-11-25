local hover = require("lib.hover")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local modal = {}

function modal.frame(title, opts)
  local title_runs = { { text = " " .. title .. " " } }
  if opts.crumbs and opts.crumbs ~= "" then
    title_runs[#title_runs + 1] = { text = opts.crumbs .. " ", style = { fg = theme.muted } }
  end
  return {
    float = { cols = opts.cols or 60, rows = opts.rows },
    type = "box",
    frame = {
      title = title_runs,
      borders = "all",
      border_style = opts.border or { fg = theme.role("modal_border") },
      style = { bg = theme.role("modal_bg") },
    },
    children = opts.children,
  }
end

function modal.footer(hints, primary, opts)
  opts = opts or {}
  local spans = {}
  for _, pair in ipairs(hints) do
    spans[#spans + 1] = { text = pair[1], style = { fg = theme.hint } }
    spans[#spans + 1] = { text = " " .. pair[2] .. "  ", style = { fg = theme.muted } }
  end
  local hint_row = { type = "text", fill = 1, text = { spans } }
  local children = { opts.stack and { type = "text", fill = 1, text = "" } or hint_row }
  local function pill(label, role, style, inert)
    local lit = not inert and hover.role(role)
    return {
      type = "text",
      len = widgets.len(label) + 1,
      text = { { { text = " " }, { text = label, style = lit and hover.button_style() or style } } },
      role = role,
    }
  end
  if primary then
    children[#children + 1] = pill(
      "[ " .. primary .. " ]",
      "key:" .. (opts.key or "enter"),
      opts.disabled and { fg = theme.muted } or opts.style or { fg = theme.accent, bold = true },
      opts.disabled
    )
  end
  children[#children + 1] =
    pill("[ " .. (opts.cancel or "Cancel") .. " ]", "key:esc", { fg = theme.muted })
  local pills = { type = "box", axis = "horizontal", len = 1, children = children }
  if opts.stack then
    hint_row.len, hint_row.fill = 1, nil
    return { type = "box", len = 2, children = { hint_row, pills } }
  end
  return pills
end

return modal
