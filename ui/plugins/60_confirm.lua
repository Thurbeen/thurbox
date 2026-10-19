-- The confirmation, for a change that cannot be undone.
--
-- A pane that is about to do something irreversible leaves the question in
-- `store.confirm`:
--
--   store.confirm = {
--     question = "Delete fix-osc52 and its worktree?",
--     lines = { "3 uncommitted files", "2 commits not on origin" },
--     command = "delete",
--     options = { session = id, force = true },
--   }
--
-- and this asks it, then issues that command on `y`/`enter`. The pane that owns
-- the data decides *what* is at risk — which is why the session list can itemise
-- a worktree's state without a confirmation dialog knowing what a worktree is.
--
-- What `y` does is worded here, from the command, and not taken from the
-- question: `store` is written by every pane and this one holds `sessions`, so a
-- question is only its author's words. The pane that asked is named beside it.
--
-- v1 has two bespoke modals for this (`ConfirmDelete`, `ConfirmRestore`), each
-- with the risk assessment welded in.

local modal = require("lib.modal")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

--- The commands a question may end in.
---
--- `store` is written by every pane, and this one holds `sessions` because it
--- ships with thurbox — so anything it would forward, any pane could issue
--- through it. A `send` or a `create` behind a question its author wrote is the
--- capability laundered, so only the changes the shipped panes ask about pass.
local FORWARDS = { delete = true, restart = true, restore = true, sync = true }

--- A session's name, from the live list or the restore list, for the line
--- that says what will happen to it.
local function session_name(id)
  for _, rows in ipairs({ thurbox.sessions or {}, thurbox.deleted or {} }) do
    for _, row in ipairs(rows) do
      if row.id == id then
        return row.name or id
      end
    end
  end
  return id or "a session"
end

--- What `y` will do, in words made here from the command it issues.
local function consequence(kind, options)
  local name = session_name(options.session)
  if kind == "delete" then
    return options.force and ("Delete " .. name .. " and its worktree for good")
      or ("Delete " .. name)
  elseif kind == "restart" then
    return "Restart " .. name
  elseif kind == "restore" then
    return options.force and ("Restore " .. name .. " with only its committed work")
      or ("Restore " .. name)
  end
  return "Sync " .. name .. " with the branch it came from"
end

--- The request this last drew, which is the one `y` issues.
---
--- Not `store.confirm` read again at the keypress: a float drawn after this one
--- could replace it between the frame the user read and their answer, and `y`
--- would then issue a request nobody saw.
local shown = nil

--- The pending question, if a pane left one it may ask.
local function pending()
  local ask = store.confirm
  if type(ask) ~= "table" or not ask.question or not FORWARDS[ask.command] then
    return nil
  end
  return ask
end

local function clear()
  store.confirm = nil
  shown = nil
end

return {
  name = "confirm",
  -- A force delete and a restart are issued from here, on the user's `y`.
  -- Granted without asking while this file is the one thurbox ships.
  capabilities = { "sessions" },
  ui_state = function()
    return { open = pending() ~= nil }
  end,
  -- A slot the arrangement never places: this only ever floats.
  slot = "float",
  order = 60,
  floats = true,
  -- This render reads `store.confirm` and the theme and writes nothing but
  -- `shown`, which a reused tree leaves matching what is on screen, so the
  -- kernel may reuse the tree — and floats render every frame even while
  -- closed, so without this the closed state costs a Lua call per frame
  -- forever. `store.confirm` writes bump the state version, which is part of
  -- the cache key, so opening and answering stay on the very next frame.
  pure = true,
  -- Never a tab stop: it is up only while it has a question, and it takes every
  -- key while it is.
  focusable = false,

  render = function(_)
    local ask = pending()
    shown = ask
    if not ask then
      return { type = "text", text = "" }
    end

    local options = type(ask.options) == "table" and ask.options or {}
    local children = {
      {
        type = "text",
        len = 1,
        text = {
          {
            {
              text = " " .. consequence(ask.command, options) .. "?",
              style = { fg = theme.text, bold = true },
            },
          },
        },
      },
      {
        type = "text",
        len = 1,
        text = {
          {
            {
              text = " asked by " .. (store_writer("confirm") or "thurbox"),
              style = { fg = theme.muted },
            },
          },
        },
      },
    }

    -- What is at risk, itemised by whoever asked. A question with nothing listed
    -- is not padded out — the absence says the change is cheap.
    local lines = ask.lines
    if type(lines) == "table" and #lines > 0 then
      children[#children + 1] = { type = "text", len = 1, text = "" }
      for _, line in ipairs(lines) do
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { { { text = "   • " .. line, style = { fg = theme.bad } } } },
        }
      end
    end

    children[#children + 1] = { type = "text", len = 1, text = "" }
    -- The Confirm pill wears the danger colour and replays `y`, not enter: a
    -- destructive yes should look like one and mean exactly the key it names.
    children[#children + 1] = modal.footer(
      { { " y", "confirm" }, { "esc", "cancel" } },
      "Confirm",
      { key = "y", style = { fg = theme.bad, bold = true } }
    )

    local rows = 2
    for _, child in ipairs(children) do
      rows = rows + (child.len or 1)
    end
    -- Danger-bordered, as v1 frames a destructive confirmation.
    return modal.frame("Confirm", {
      cols = 60,
      rows = rows,
      border = { fg = theme.bad },
      children = children,
    })
  end,

  on_key = function(key)
    -- Still pending, and answered with what was on screen.
    local ask = pending() and shown
    if not ask then
      return false
    end
    if key.key == "esc" or key.key == "n" then
      clear()
      return true
    end
    if key.key == "y" or key.key == "enter" then
      -- Cleared BEFORE the command is issued: the command lands in a later
      -- snapshot, and a question left up meanwhile would invite a second yes.
      local kind, options = ask.command, ask.options
      clear()
      command(kind, type(options) == "table" and options or {})
      return true
    end
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
    return true
  end,

  --- A click anywhere that is not a pill dismisses nothing and confirms nothing;
  --- the pills carry `key:` roles and never reach here. Declared so the float's
  --- own rect does not fall through to the pane beneath it.
  on_click = function(_)
    return widgets ~= nil
  end,
}
