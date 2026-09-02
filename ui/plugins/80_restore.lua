-- The restore list: sessions that were deleted and not yet purged.
--
-- v1's `Ctrl+U` modal (`RestoreSessionsModal`), with v1's division of labour
-- kept intact: `Ctrl+Z` in the session list undoes the delete YOU just did,
-- while this one lists every row the database still holds — including ones
-- another instance deleted, and ones deleted before thurbox was last restarted.
--
-- It knows nothing about how a restore works. `thurbox.deleted` is the read and
-- `restore` is the command; whether a row's worktrees can be reattached is the
-- kernel's problem, and whether a force-deleted one SHOULD be is a question put
-- through `store.confirm` — the same confirmation the session list uses for a
-- delete, which is why there is no bespoke `ConfirmRestore` here.

local modal = require("lib.modal")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

--- The float's width, asked for in CELLS rather than as a share of the screen.
---
--- v1 sizes this modal at 60% (`render_list_modal_frame(frame, 60, …)`) and the
--- kernel takes either, but a row has to be truncated against the frame that will
--- hold it — and a share is not resolvable here: a float is rendered with the
--- whole screen as its probe rect and only placed afterwards. Cells are known,
--- and the kernel clamps an over-ambitious one to the screen.
local MODAL_COLS = 60

--- The columns a row inside the float actually gets: the frame's own border on
--- each side, then the selection marker `widgets.list` draws.
local function row_cols(cols)
  return math.max(1, cols - 4)
end

--- Rows shown at once. More than this and the list windows, which
--- `widgets.list` does with the overflow markers already.
local LIST_MAX = 10

local function deleted()
  return (thurbox and thurbox.deleted) or {}
end

local function is_open()
  return state.open == true
end

local function close()
  state.open = nil
  state.cursor = nil
end

--- The selection, always resolved against the list it indexes: rows leave
--- `thurbox.deleted` as they are restored, so a stored cursor outlives the row
--- it pointed at.
local function cursor_in(list)
  return widgets.clamp(state.cursor or 1, #list)
end

--- One row, in v1's spelling: `name (agent) 3m ago [wt]`.
---
--- A force-deleted row is dimmed and tagged, because the tag is the difference
--- between "this comes back" and "the committed half of this comes back" — and
--- that has to be readable BEFORE the choice, not in the confirmation after it.
local function row_spans(entry, cols)
  local base = entry.partial and theme.muted or theme.text
  local text = entry.name .. " (" .. (entry.agent or "") .. ")"
  if entry.deleted_at then
    text = text .. " " .. widgets.time_ago(entry.deleted_at)
  end
  if (entry.worktrees or 0) > 0 then
    text = text .. " [wt]"
  end
  local tag = entry.partial and " force-deleted" or ""
  -- Truncated against the columns a row really has, so the tag is never what
  -- gets clipped: it is the part of the row that changes what Enter means.
  text = widgets.truncate(text, math.max(1, row_cols(cols) - widgets.len(tag)))
  local spans = { { text = text, style = { fg = base } } }
  if entry.partial then
    spans[#spans + 1] = { text = tag, style = { fg = theme.bad } }
  end
  return spans
end

--- v1's `key_hint_line` plus its `[ Restore ]` / `[ Close ]` pills — the
--- shared `modal.footer`, with the Restore pill offered only while there is a
--- row for Enter to act on.
local function footer(list)
  local hints = {}
  if #list > 0 then
    hints[#hints + 1] = { "j/k", "navigate" }
  end
  hints[#hints + 1] = { "esc", "close" }
  return modal.footer(hints, #list > 0 and "Restore" or nil, { cancel = "Close" })
end

--- Ask before a restore the kernel would otherwise refuse, through the
--- confirmation every other irreversible-ish choice goes through.
---
--- v1 has a whole modal for this (`ConfirmRestore`) whose only content is the
--- sentences below.
---
--- The question is asked when `restore_refusal` is set, not when the row is
--- tagged force-deleted: the two differ in both directions (a force-deleted
--- row whose worktrees were only borrowed restores outright; a soft-deleted
--- one whose borrowed checkout is gone cannot). The reason is the kernel's own
--- sentence rather than one written here, so the pane cannot describe a
--- refusal it does not decide.
local function confirm_refusal(entry)
  store.confirm = {
    question = "Restore '" .. entry.name .. "' anyway?",
    lines = {
      entry.restore_refusal,
      entry.partial and "only committed work on its branch is reattached" or nil,
    },
    command = "restore",
    -- `force` is the wire name for the restore command's `best_effort`: the
    -- kernel refuses without it, and this question is the caller saying yes.
    options = { session = entry.id, force = true },
  }
end

return {
  name = "restore",
  -- A slot the arrangement never places: this only ever floats. It is a list of
  -- sessions that are NOT there, so it has no business taking a pane's rect.
  slot = "float",
  order = 80,
  floats = true,
  -- This render reads `thurbox.deleted`, `state` and the theme and writes
  -- nothing, so the kernel may reuse the tree — and floats render every frame
  -- even while closed, so without this the closed state costs a Lua call per
  -- frame forever. `state.open`/`state.cursor` writes bump the state version,
  -- which is part of the cache key, and `thurbox.deleted` rides the snapshot
  -- version, so opening and refreshing stay on the very next frame.
  pure = true,
  -- Never a tab stop: it is up only while it is open, and it takes every key
  -- while it is.
  focusable = false,

  keys = {
    -- v1's `Ctrl+U`, and like v1 a PASSTHROUGH chord: `Ctrl+U` is readline's
    -- kill-line, so a focused agent keeps the keystroke and the list stays
    -- reachable from every other pane.
    {
      key = "ctrl+u",
      action = "restore.open",
      desc = "restore a deleted session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    -- Plugin-scoped, so they fire only while the list is up. Declared rather
    -- than merely handled, which is what puts them in help and makes them
    -- rebindable.
    { key = "j", action = "restore.next", desc = "next session", group = "Restore" },
    { key = "k", action = "restore.previous", desc = "previous session", group = "Restore" },
    { key = "down", action = "restore.next", desc = "next session", group = "Restore" },
    { key = "up", action = "restore.previous", desc = "previous session", group = "Restore" },
  },

  render = function(ctx)
    if not is_open() then
      return { type = "text", text = "" }
    end
    -- A float is probed with the whole screen, so this is the screen's width and
    -- the clamp the kernel will apply to the float is applied to the rows too.
    local cols = math.min(MODAL_COLS, math.max(4, ctx.width or MODAL_COLS))
    local list = deleted()
    local cursor = cursor_in(list)
    -- Spans only for the visible window — the same `widgets.window` call the
    -- list itself makes, so the two agree; off-window entries are placeholders
    -- whose spans the list never reads. The deleted list can be long after an
    -- unpurged week, and it shows ten rows.
    local height = math.max(1, math.min(#list, LIST_MAX))
    local first, last = widgets.window(#list, height, cursor)
    local rows = {}
    for index, entry in ipairs(list) do
      if index >= first and index <= last then
        rows[index] = { spans = row_spans(entry, cols), id = entry.id }
      else
        rows[index] = { spans = {}, id = entry.id }
      end
    end
    local body = widgets.list({
      rows = rows,
      selected = cursor,
      height = height,
      empty = "  No deleted sessions",
    })
    body.len = height

    return modal.frame("Restore Deleted Sessions", {
      cols = cols,
      rows = height + 3,
      children = { body, footer(list) },
    })
  end,

  on_action = function(action)
    if action == "restore.open" then
      -- The opening chord toggles, as the kernel's own modals do. There is no
      -- half-filled state to lose here — the list is a read.
      if is_open() then
        close()
      else
        state.open = true
        state.cursor = 1
      end
      return true
    end
    if not is_open() then
      return false
    end
    local list = deleted()
    if action == "restore.next" then
      state.cursor = widgets.clamp(cursor_in(list) + 1, #list)
      return true
    elseif action == "restore.previous" then
      state.cursor = widgets.clamp(cursor_in(list) - 1, #list)
      return true
    end
    return false
  end,

  --- What a modal owns without declaring: confirm and cancel. v1 keeps the same
  --- two fixed rather than rebindable.
  on_key = function(key)
    if not is_open() then
      return false
    end
    if key.key == "esc" then
      close()
      return true
    end
    if key.key == "enter" then
      local list = deleted()
      local entry = list[cursor_in(list)]
      if not entry then
        -- An empty list still swallows the key: a modal that acted on nothing
        -- would be indistinguishable from one that failed.
        return true
      end
      -- Closed BEFORE the command is issued: the restored row leaves
      -- `thurbox.deleted` a snapshot later, and a list left up meanwhile would
      -- invite a second Enter on a row that is already coming back.
      close()
      if entry.restore_refusal then
        confirm_refusal(entry)
      else
        command("restore", { session = entry.id })
      end
      return true
    end
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
    return true
  end,

  --- A click selects the row it landed on, exactly as `j`/`k` would. The pills
  --- carry `key:` roles and never reach here.
  on_click = function(hit)
    if not is_open() then
      return false
    end
    if hit.id then
      local index = widgets.index_of(deleted(), hit.id)
      if index then
        state.cursor = index
      end
    end
    -- Claimed either way, so a miss inside the float does not fall through to
    -- the pane beneath it.
    return true
  end,
}
