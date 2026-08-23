-- The modal float shell, shared by every floating dialog.
--
-- Three panes float one — the creation flow, the confirmation, the restore
-- list — and each needs the same two pieces: the framed float itself, and the
-- key-hint footer with its clickable pills. Each had grown its own copy, and
-- the copies had already drifted (one clipped its Cancel pill by a column);
-- this module is the one spelling.
--
-- Sizing is in CELLS (`float.cols`), never a share of the screen: every row
-- inside a modal is truncated against a column budget, and a percentage float
-- clips rows on a narrow terminal and leaves dead space on a wide one.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

local modal = {}

--- The framed float.
---
--- opts:
---   cols     — width in cells (default 60)
---   rows     — height in rows
---   children — the modal's body
---   crumbs   — muted text appended to the title (the flow's breadcrumb
---              trail). Titles carry runs, so it keeps its own colour while
---              the rest of the title inherits the border's.
---   border   — border colour override (the confirmation's danger frame);
---              defaults to the `modal_border` role
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

--- v1's `key_hint_line` plus its `[ Done ]` / `[ Cancel ]` pills.
---
--- `hints` is a list of `{ key, description }` pairs; `primary` labels the
--- confirm pill, or nil to offer no confirm at all (a list with nothing to act
--- on still needs its Close). opts: `key` is the keystroke the primary pill
--- replays (default "enter"), `style` its colour (default accent), `cancel`
--- the dismiss pill's label (default "Cancel" — its key is always esc).
---
--- The pills carry `key:` roles, so a click replays the very keystroke they
--- name — a button and its key cannot come to mean different things.
---
--- Both pills carry a LEADING space and are measured from the string itself.
---
--- `" [ Cancel ]"` is eleven columns and the slot was once hard-coded to ten,
--- so the closing bracket was clipped off at every step of the flow. And the
--- leading space belongs to the pill rather than to the hints: the hints take
--- `fill = 1`, so once they are long enough to use their whole share their own
--- trailing padding is what gets truncated — which is how `d forget[ Done ]`
--- ran together with no gap at all.
function modal.footer(hints, primary, opts)
  opts = opts or {}
  local spans = {}
  for _, pair in ipairs(hints) do
    spans[#spans + 1] = { text = pair[1], style = { fg = theme.hint } }
    spans[#spans + 1] = { text = " " .. pair[2] .. "  ", style = { fg = theme.muted } }
  end
  local children = { { type = "text", fill = 1, text = { spans } } }
  if primary then
    local done = " [ " .. primary .. " ]"
    children[#children + 1] = {
      type = "text",
      len = widgets.len(done),
      text = { { { text = done, style = opts.style or { fg = theme.accent, bold = true } } } },
      role = "key:" .. (opts.key or "enter"),
    }
  end
  local cancel = " [ " .. (opts.cancel or "Cancel") .. " ]"
  children[#children + 1] = {
    type = "text",
    len = widgets.len(cancel),
    text = { { { text = cancel, style = { fg = theme.muted } } } },
    role = "key:esc",
  }
  return { type = "box", axis = "horizontal", len = 1, children = children }
end

return modal
