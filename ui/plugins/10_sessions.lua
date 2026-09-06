-- The session list.
--
-- In v1 this was `ui::project_list` — 2,212 lines of Rust, welded to a 616-method
-- `App` struct. Here it is a file you can edit while thurbox is running.
--
-- It is an ORDINARY plugin. The kernel has no session-list concept: it hands
-- over a snapshot and this decides everything about how the list looks.
--
-- This file reproduces v1's rendering cell for cell: the `── repo ──` rule
-- headers, the `└`/`↳`/`⇅`/`⑂` marks, the full-width selection bar, the status
-- dot strip on the top border, and the `▲ N`/`▼ N` scroll indicators overlaid on
-- the border rows.
--
-- The border is an ordinary kernel `frame`, and this file no longer spells it:
-- `ui.panel` does. It was drawn by hand, out of a cell buffer, for as long as a
-- frame title was a plain unstyled left-aligned string — a frame could express
-- none of the three things v1 puts on this border. It can now: the title is
-- styled runs, so the focused badge is a title; the dot strip and the scroll
-- counts are `frame.overlay`, painted onto the border cells after the block,
-- which is what keeps them off the content rows.
--
-- What is left here is the DECISIONS — which glyph, which colour role, when to
-- drop a trailing status, what a repo header says — while the window
-- arithmetic, the selection bar, the empty state and the focus border are
-- `lib/ui`'s, shared with every other pane.

local fuzzy = require("lib.fuzzy")
local order = require("lib.order")
local panels = require("lib.panels")
local plugin_settings = require("lib.settings")
local session_model = require("lib.session_model")
local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

-- ── The model, the components and the ordering algebra live in lib/ ─────────
--
-- `lib.session_model` builds the item list (one selectable unit per row, with
-- the group header glued to its group's first session), `lib.ui` is the
-- component layer — the panel, the list, the cursor and the row builder, and
-- with them the window arithmetic, the selection bar and the focus border this
-- pane used to spell itself — and `lib.order` is the move/sort algebra over the
-- rendered items. All three are pure over what this pane hands them; everything
-- about how a row LOOKS stays here.

-- ── Turning a model item into lines ────────────────────────────────────────

--- The status text that follows the name. v1 shows the agent's notification (or
--- the word "Blocked") for a blocked row, and the OSC activity title otherwise;
--- a row with neither carries no text, because the coloured dot already says
--- what state it is in.
---
--- Both come off the agent's own terminal — the activity line is its OSC window
--- title, the notification its OSC 9/777 message — so they are published from
--- the live pane rather than the database.
---
--- v2 adds the row nothing has reported for. Its dot says only that no status
--- arrived, which is honest but not diagnosable, so the text names what thurbox
--- does know: the agent found in the pane, or why the silence means nothing.
--- Naming the agent never displaces what it said — an activity line is the
--- agent talking, and it wins the rest of the line.
local function agent_status_text(session)
  -- Nothing the agent said can be current on a host we cannot reach, so the
  -- row says why instead of showing a last message as if it were live.
  if session.status == "unreachable" then
    if session.remote_host then
      return "host " .. session.remote_host .. " unreachable"
    end
    return "unreachable"
  end
  if session.status == "blocked" then
    return session.notification or "Blocked"
  end
  local activity = session.activity
  if activity then
    activity = activity:match("^%s*(.-)%s*$")
    if activity == "" then
      activity = nil
    end
  end
  -- An agent thurbox did not launch: the row is labelled with whatever the
  -- driver asked for (`zsh`), and the agent actually in front of the user is
  -- named here. It says WHICH agent, never what it is doing — the dot already
  -- says that nothing has reported.
  local detected = session.detected_agent
  if detected then
    if activity then
      return detected .. " · " .. activity
    end
    return detected .. " · no status reported"
  end
  if activity then
    return activity
  end
  if session.status == "uncovered" then
    return "no status hooks"
  end
  if session.status == "unreported" then
    return "no status reported"
  end
  return nil
end

--- The live search query, or nil when nothing is being searched.
---
--- Read from `store` rather than handed over by the search pane: the pane
--- holding a row is what knows how a highlight should look in it, so search
--- publishes WHAT it is looking for and each pane answers for its own rows. The
--- kernel's `decorate` hook does the same job for panes search cannot expect to
--- cooperate; this list is one of its own.
local function search_query()
  local text = store["search.query"]
  if type(text) ~= "string" or text == "" then
    return nil
  end
  return text
end

--- Where the query hits a session's name, or nil when the row does not match at
--- all. `false` means it matched on something else — the row stays lit but its
--- name carries no marks.
---
--- `search` is the render-level `{ text, needle }` pair: the needle is compiled
--- once per render rather than re-split per field per row.
local function name_hits(session, search)
  if not search then
    return nil
  end
  local positions = fuzzy.match(search.needle, session.name or "")
  if positions then
    return positions
  end
  -- `or ""` on every element, because `ipairs` STOPS AT THE FIRST NIL. A session
  -- with no worktree has `session.branch == nil` in the middle of this list, so
  -- the scan halted after `agent` and its repository was never tested — the row
  -- was then dimmed as a non-match while the search strip counted it as a match
  -- and annotated it `repo: …`. The two halves of one feature disagreed on the
  -- same frame, and only for sessions without a branch, which is why the one
  -- worktree session in a review capture looked like the only correct row.
  for _, field in ipairs({ session.agent or "", session.branch or "", session.repo or "" }) do
    if field ~= "" and fuzzy.match(search.needle, field) then
      return false
    end
  end
  return nil
end

--- The spans of one session row.
---
--- The row's own `style` — the selection bar, the hover band — is `ui.list`'s,
--- not this pane's: it is the one selection idiom the whole interface uses, and
--- a pane that spelled its own would be the fourth spelling. What is still
--- decided here is every colour a span asks for, and `tone` is where a row says
--- the bar speaks for all of them.
local function session_line(item, width, elapsed, is_selected, work, search)
  local session = item.session
  local spec = ui.status(session.status, elapsed)
  local glyph, glyph_color = spec.glyph, spec.color
  -- A blocked row's text is an attention message, so it keeps the dot's colour;
  -- plain activity is muted, leaving the name the row's visual anchor. v1 draws
  -- the same split.
  local trailing = agent_status_text(session)
  local trailing_color = glyph_color
  if session.status ~= "blocked" then
    trailing_color = theme.muted
  end

  -- Work already accepted but not yet in the snapshot is the more recent truth,
  -- so it takes the dot's place. v1 has no equivalent for a live row; the
  -- geometry is v1's, the signal is v2's.
  if work then
    if work.phase == "failed" then
      glyph, glyph_color = "✗", theme.role("status_error")
      trailing = work.error and ("failed: " .. work.error) or "failed"
      trailing_color = theme.role("status_error")
    else
      glyph, glyph_color = "◌", theme.muted
      trailing, trailing_color = work.kind, theme.muted
    end
  end

  local hits = name_hits(session, search)

  local row = ui.row({
    width = width,
    --- The colour a span wears, unless the ROW speaks for all of them.
    ---
    --- Selected: nothing names a foreground, so every cell takes the bar's —
    --- a span that named one would poke a hole in it. Unmatched: v1 keeps a
    --- non-matching row on screen and lets the contrast do the filtering, so
    --- the list never jumps around under a cursor you are still moving.
    tone = function(style)
      if is_selected then
        return nil
      end
      if search and hits == nil then
        return { fg = theme.muted }
      end
      return style
    end,
  })

  row:add(" " .. glyph .. " ", { fg = glyph_color })

  -- Nesting prefix: a tree mark for a child inside the group, a lone mark for
  -- one whose parent renders elsewhere in the list.
  if item.depth > 0 then
    row:add(string.rep("  ", item.depth - 1) .. "└ ", { fg = theme.muted })
  elseif item.cross_group then
    row:add("↳ ", { fg = theme.muted })
  end

  -- An agent running on another machine, then a session that owns a worktree.
  if session.host then
    row:add("⇅ ", { fg = theme.accent })
  end
  if (session.worktrees or 0) > 0 then
    row:add("⑂ ", { fg = theme.branch })
  end

  -- Never truncated: the name is the row's anchor, and overflow clips.
  --
  -- A matched run is the one thing that keeps its colour on a selected row: it
  -- names a foreground, and the bar underneath supplies only what a span left
  -- unsaid. v1 layered the same two the same way round — `highlight_style` was
  -- built ON TOP of the row's base style (`src/ui/highlight.rs`) — and
  -- previewing a result moves this list's cursor onto the row, so the selected
  -- row is exactly the one whose marks would otherwise disappear.
  row:match(session.name or "?", hits, { fg = theme.text }, {
    fg = theme.accent,
    bold = true,
    underline = true,
  })

  return row:trailing(trailing, { fg = trailing_color }):spans_list()
end

--- v1's phase vocabulary, which the placeholder row shows beside the label.
local PHASE_LABEL = {
  queued = "creating…",
  running = "creating…",
  resolving = "setting up…",
  hooks = "running hooks…",
  host = "on the host…",
  worktrees = "creating…",
  backend = "setting up…",
  launching = "spawning…",
  persisting = "spawning…",
}

local function pending_line(work, width, elapsed)
  local failed = work.phase == "failed"
  local glyph, glyph_style
  if failed then
    glyph, glyph_style = "✗", { fg = theme.role("status_error") }
  else
    -- A spinner only while something is actually running.
    glyph, glyph_style = theme.spinner_frame(elapsed), { fg = theme.warn }
  end

  local label = work.subject or "new session"
  local phase
  if failed then
    phase = work.error and ("failed: " .. work.error) or "failed"
  else
    phase = PHASE_LABEL[work.phase] or "creating…"
  end

  local row = ui.row({ width = width })
  row:add(" " .. glyph .. " ", glyph_style)
  row:add(label, { fg = theme.secondary })
  -- Drop the phase rather than overflow a narrow panel. Not `row:trailing`:
  -- that keeps a note only when four columns are left for it, and a creation
  -- phase is either shown whole or not at all.
  if width > row.used + widgets.len(phase) + 2 then
    row:add("  " .. phase, { fg = theme.muted })
  end
  return row:spans_list()
end

local function sessions()
  return thurbox and thurbox.sessions or {}
end

--- Is deleting reversible?
---
--- `[features] soft_delete` off means the TUI deletes for real — v1's own
--- behaviour, and why the confirmation below exists: there is no Ctrl+Z for it.
local function soft_delete()
  return plugin_settings.feature("soft_delete", true) ~= false
end

--- Does a session this interface just created take the cursor and the keyboard?
---
--- Off by default, because a spawn finishes on a worker seconds after the flow
--- closed: the moment the row lands is not a moment you chose, and being moved
--- then interrupts whatever you went back to reading. Turned on, it saves
--- hunting for the row you just asked for — which is the whole of the trade,
--- so it is yours to make rather than ours.
local function focus_new_session()
  return plugin_settings.enabled("sessions", "focus_new_session", false)
end

--- What deleting this session would destroy, itemised — or nil when it would
--- destroy nothing.
---
--- v1's `DeleteRisk::from_stats`, its decision included: work is at risk when
--- there are uncommitted or untracked files, commits that exist nowhere else,
--- or — the case that matters most — a state that could not be read at all,
--- which is reported rather than assumed clean. Everything else is a
--- known-clean session, and nil says so.
---
--- The worktree *directory* is deliberately not a reason to ask: force-delete
--- removes the checkout and leaves the branch, so a clean one comes back from
--- it. It is listed as context for a question already owed, never as the cause.
local function at_risk(session)
  local git = session.git
  if not git then
    -- Not computed yet, not a git worktree, or a host that could not be
    -- reached. v1 confirms rather than assume clean.
    return { "its state could not be read" }
  end

  local lines = {}
  local uncommitted = (git.files or 0) + (git.untracked or 0)
  if uncommitted > 0 then
    lines[#lines + 1] = uncommitted .. " uncommitted or untracked file(s)"
  elseif git.dirty then
    -- `dirty` is any `status --porcelain` output, so it outlives a count of
    -- zero (a mode change, a submodule). Still work, still unrecoverable.
    lines[#lines + 1] = "uncommitted changes"
  end
  -- Commits are only at risk while they exist nowhere but here. A merged
  -- branch keeps its ahead count forever — a squash or a rebase-and-merge
  -- rewrites the work into new commits, so none of these are ancestors of the
  -- default branch and the count never falls back to zero once the remote
  -- branch is gone. `merged` compares trees and patches, so it sees the work on
  -- origin's default whichever way the forge landed it; anything short of a
  -- confirmed `true` keeps the question.
  if (git.ahead or 0) > 0 and git.merged ~= true then
    lines[#lines + 1] = git.ahead .. " commit(s) not pushed anywhere else"
  end

  -- Everything above speaks for the session's *primary* directory, which is the
  -- only one the snapshot stats. v1 inspected every worktree it was about to
  -- remove, so on a session that owns several the rest are unknown rather than
  -- clean — a reason to ask in its own right.
  local worktrees = session.worktrees or 0
  if worktrees > 1 then
    lines[#lines + 1] = "its other worktrees could not be read"
  end

  if #lines == 0 then
    return nil
  end

  -- What else goes, once a question is owed. Never the reason for one.
  if worktrees == 1 then
    lines[#lines + 1] = "its worktree directory"
  elseif worktrees > 1 then
    lines[#lines + 1] = "its " .. worktrees .. " worktree directories"
  end
  return lines
end

--- Delete for good, asking first only when there is something to lose.
---
--- v1's `App::delete_active_session`: it assessed the risk and opened
--- `ConfirmDelete` only for `Some(risk)`, deleting a known-clean session on the
--- keystroke. Asking about nothing trains the answer, which is the opposite of
--- what a confirmation is for.
---
--- The question travels through `store`, so the confirm plugin needs to know
--- nothing about sessions.
local function delete_for_good(session, question)
  local lines = at_risk(session)
  if not lines then
    command("delete", { session = session.id, force = true })
    return
  end
  store.confirm = {
    question = question,
    lines = lines,
    command = "delete",
    options = { session = session.id, force = true },
  }
end

--- Persist a rendered order. Header ownership is a *rendering* property of the
--- first row in a group, so it is left to the next build rather than carried.
--- This list's cursor, in the one spelling every handler here reads it with.
---
--- `target` is a model item's identity — nil on a group header, `false` on work
--- with no session yet — so a row that selects nothing is skipped by
--- construction rather than by each caller checking. `steer` is the `store` key
--- another pane writes to move this list; `request` is the one-shot
--- `focus_session` a clicked notification or `thurbox-cli session focus` leaves,
--- and it is read only by `render` because consuming it anywhere else would
--- spend it on a frame that is not being drawn.
local CURSOR_OPTS = { id = "target", steer = "selected" }
local CURSOR_OPTS_WITH_REQUEST = { id = "target", steer = "selected", request = "focus_session" }

local function persist_order(items)
  local ids = {}
  for _, item in ipairs(items) do
    if item.target then
      ids[#ids + 1] = item.target
    end
  end
  if #ids > 0 then
    command("order", { list = ids })
  end
end

return {
  name = "sessions",
  slot = "sessions",
  order = 10,
  focusable = true,
  -- This render reads `thurbox.*` and `ctx` and writes nothing, so the kernel
  -- may reuse the tree it returned while neither has changed. The working
  -- spinner still animates: the kernel drops the cached tree when the shared
  -- animation clock moves, and that clock ticks at the same rate
  -- `theme.spinner_frame` advances the spinner at — but only while something is
  -- actually animating, so an idle list settles instead of re-rendering.
  pure = true,

  -- Declared as DATA, not just handled. That is what lets the kernel list these
  -- in help, detect a clash with another plugin, and let you rebind them —
  -- none of which it could do if they only existed inside on_key.
  --- Declared as data, so the settings modal renders a row for each without
  --- knowing what a repo group is or what creating a session does. Read back
  --- through `lib.settings`.
  settings = {
    {
      id = "group_by_repo",
      desc = "Group sessions under a repo header",
      default = true,
    },
    {
      id = "focus_new_session",
      desc = "Select and open a session when you create or fork it",
      default = false,
    },
  },

  -- The one event this pane needs: a create or a fork THIS interface finished.
  -- A session `thurbox-cli`, an automation or another instance made arrives as
  -- `session.created` instead, and subscribing to that would let a background
  -- spawn take the keyboard out from under you — so it deliberately is not
  -- subscribed to.
  events = { "session.post_create" },

  keys = {
    { key = "j", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "k", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    -- The arrows alongside j/k, which is what v1 binds by default
    -- (`Action::SessionListNext` = `j` and `Down`). Two chords for one action, so
    -- both appear in help and either can be rebound on its own.
    { key = "down", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "up", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    { key = "g", action = "sessions.first", desc = "first session", group = "Navigation" },
    -- v1's `Enter` on a session row: go to what you selected. The row is already
    -- selected by the time this fires, so opening is only a focus change.
    { key = "enter", action = "sessions.open", desc = "open the session", group = "Navigation" },
    -- Delete has no unmodified chord on purpose: `d` sits next to `j`/`k`, and
    -- a stray keystroke on a focused list should not tear a session down.
    -- `ctrl+d` below is the way in; `D` takes the worktree with it.
    {
      key = "D",
      action = "sessions.force_delete",
      desc = "delete session and its worktree",
      group = "Sessions",
    },
    { key = "r", action = "sessions.restart", desc = "restart session", group = "Sessions" },
    { key = "J", action = "sessions.move_down", desc = "move session down", group = "Sessions" },
    { key = "K", action = "sessions.move_up", desc = "move session up", group = "Sessions" },
    { key = "S", action = "sessions.sort", desc = "sort sessions by name", group = "Sessions" },

    -- v1's global session chords, which fire from any pane. Every one of them
    -- is in v1's `Action::terminal_passthrough` set — they are readline's
    -- (Ctrl+D EOF, Ctrl+R reverse-search, Ctrl+S XOFF, Ctrl+F forward-char,
    -- Ctrl+O operate-and-get-next) — so a focused agent terminal keeps the
    -- keystroke and the command stays reachable from every other pane.
    {
      key = "ctrl+d",
      action = "sessions.delete",
      desc = "delete session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+r",
      action = "sessions.restart",
      desc = "restart session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+f",
      action = "sessions.fork",
      desc = "fork session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+s",
      action = "sessions.sync",
      desc = "sync worktree with its base branch",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+o",
      action = "sessions.editor",
      desc = "open the session's directory in your editor",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    -- Not passthrough, matching v1: undo and session navigation are how you
    -- act on the list without leaving the terminal you are watching.
    {
      key = "ctrl+z",
      action = "sessions.undo",
      desc = "undo the last delete",
      scope = "global",
      group = "Sessions",
    },
    {
      key = "ctrl+j",
      action = "sessions.next",
      desc = "next session",
      scope = "global",
      group = "Navigation",
    },
    {
      key = "ctrl+k",
      action = "sessions.previous",
      desc = "previous session",
      scope = "global",
      group = "Navigation",
    },
    -- F9 alone, as in v1: the session column is the one panel toggle with no
    -- readline chord to collide with, so it needs no Ctrl primary and no
    -- passthrough exception.
    {
      key = "f9",
      action = "sessions.toggle_panel",
      desc = "toggle the session list",
      scope = "global",
      group = "Panels",
    },
  },

  render = function(ctx)
    -- ctx.width/height are THIS PANE's, not the screen's.
    local width = math.max(0, ctx.width or 0)
    local height = math.max(0, ctx.height or 0)
    if width < 2 or height < 2 then
      return { type = "text", text = "" }
    end
    local inner_width = width - 2

    local items = session_model.build(sessions())
    local busy = session_model.pending()
    -- The live query, read once per render and compiled once: `session_line`
    -- runs per visible row, and each used to re-read the store and re-split
    -- the query per field.
    local query = search_query()
    local search = query and { text = query, needle = fuzzy.compile(query) } or nil

    -- The cursor follows the SESSION it was on rather than a row number, is
    -- steered by another pane writing `store.selected`, and answers a focus
    -- request from outside the interface — all three written once in `ui.cursor`
    -- and shared with the two handlers below.
    local cursor = ui.cursor("sessions", items, CURSOR_OPTS_WITH_REQUEST)

    return ui.panel({
      title = "Sessions",
      focused = ctx.focused,
      -- One dot per session, in render order and in its own status colour,
      -- painted onto the top border. The scroll counts are laid over its tail
      -- by `ui.panel`, from the list's own hidden-row counts — every one of
      -- them a border cell, so none costs a row.
      overlay_right = ui.dots(items, ctx.elapsed, function(item)
        return item.kind == "session" and item.session.status or nil
      end),
      body = ui.list({
        items = items,
        cursor = cursor,
        width = inner_width,
        height = height - 2,
        on_overflow = "border",
        -- The pane is a column of its own: it holds its rows apart from the
        -- bottom border however few of them there are.
        pad = true,
        --- `── label ────────`, muted, full bleed. The header never reflects
        --- selection: highlighting belongs to the session rows alone.
        header = function(item)
          return item.header and ui.rule(item.header, inner_width) or nil
        end,
        class_of = function(item)
          return item.kind == "pending" and "pending-row" or "session-row"
        end,
        row = function(item, selected)
          if item.kind == "pending" then
            return pending_line(item.command, inner_width, ctx.elapsed)
          end
          return session_line(
            item,
            inner_width,
            ctx.elapsed,
            selected,
            busy[item.session.id],
            search
          )
        end,
        -- v1's placeholder, and its second line names the chord that creates a
        -- session — shown only while something actually answers it, so a rebind
        -- or the flow being removed cannot leave this advertising a dead key.
        empty = ui.empty({
          title = "No sessions yet",
          width = inner_width,
          hint = "Press %s to create one",
          hint_action = "new_session.open",
        }),
      }),
    })
  end,

  --- Go to a session you just made, when you asked to be taken there.
  ---
  --- The event fires once the spawn has landed and the snapshot has been
  --- re-read, so the row exists by now and the jump is a single frame.
  on_event = function(name, payload)
    if name ~= "session.post_create" or not focus_new_session() then
      return
    end
    -- No id means the row could not be resolved from the name (a spawn that
    -- landed nothing, or two sessions sharing a name). Nothing to go to, and
    -- taking the keyboard to the agent pane anyway would only re-open the
    -- session already selected.
    local id = payload.session
    if not id then
      return
    end
    -- The two halves `Enter` performs, for the row you did not have to find:
    -- the cursor follows the id — sticky until a render lands on it, and
    -- dropped the moment you move the cursor yourself — while the agent pane,
    -- the one that shows a session, takes the keyboard. `store.selected` is
    -- written here as well as followed, because a pane that draws before this
    -- one otherwise shows the previous session for a frame.
    ui.follow("sessions", id)
    store.selected = id
    command("focus", { text = "agent" })
  end,

  -- A click on a row selects it, exactly as `j`/`k` would — v1's
  -- `ClickAction::SelectSession`. The row carries the session id rather than an
  -- index, so a list that reordered between the paint and the press still
  -- selects the session you pointed at.
  --
  -- A repo header carries no id — it is drawn as its own line and only session
  -- lines are targets — so a click on one focuses the column and selects
  -- nothing. v1 folds the header into its group's first hitbox instead; giving
  -- it an id here would also hand it `role = "row"`, which the search
  -- decorator matches on, so the divergence is deliberate.
  on_click = function(hit)
    if not hit.id then
      return false
    end
    local items = session_model.build(sessions())
    return ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) ~= nil
  end,

  on_action = function(action)
    -- The two that own no row, handled before the "is there a session" guard:
    -- hiding the column and undoing a delete both work on an empty list.
    if action == "sessions.toggle_panel" then
      panels.toggle("sessions")
      return true
    elseif action == "sessions.undo" then
      -- v1's Ctrl+Z undoes the delete YOU just did — `App::undo_delete`
      -- restores its own `pending_delete` — rather than reaching for the most
      -- recently deleted row, which may belong to another instance.
      if not state.deleted then
        -- Said out loud rather than swallowed. Ctrl+Z is global: it fires
        -- from a focused terminal, and with the column hidden (F9) there is
        -- nothing on screen to tell "there was nothing to undo" from a chord
        -- that never arrived.
        command("message", { text = "nothing to undo" })
        return true
      end
      command("restore", { session = state.deleted })
      state.deleted = nil
      return true
    end

    local items = session_model.build(sessions())
    if #items == 0 then
      return false
    end
    -- The same cursor `render` builds, from the same state: moving it here
    -- republishes the selection as well, because Ctrl+J/K are global — with the
    -- column hidden (F9) or a terminal focused, this pane may not render again
    -- before the agent pane does.
    local cursor = ui.cursor("sessions", items, CURSOR_OPTS)
    local at = cursor.index
    local id = cursor:id()

    -- Actions, not chords. The kernel already resolved which key was pressed,
    -- so the capital-vs-shift encoding trap is its problem now, not ours.
    if action == "sessions.open" then
      -- The agent pane is what shows a session; focusing it is what "open" means
      -- here, exactly as v1's Enter moves focus to the terminal.
      if id then
        command("focus", { text = "agent" })
      end
    elseif action == "sessions.next" then
      cursor:move(1)
    elseif action == "sessions.previous" then
      cursor:move(-1)
    elseif action == "sessions.first" then
      cursor:select(1)

    -- Every state change below is a COMMAND: accepted instantly, its effect
    -- appearing in a later snapshot. Nothing here waits for anything.
    elseif action == "sessions.delete" and id then
      if soft_delete() then
        -- Remembered for Ctrl+Z. Only the soft delete: a force-delete removed the
        -- worktree, so there is nothing an undo could put back.
        state.deleted = id
        command("delete", { session = id })
      else
        -- The switch is off, so this key deletes for real. v1 asks first when
        -- there is work to lose, because there is no undo to fall back on.
        local session = items[at].session
        delete_for_good(session, "Delete " .. (session.name or "this session") .. " for good?")
      end
    elseif action == "sessions.force_delete" and id then
      -- Destructive, and undone by nothing — but only worth a question when it
      -- would take work with it.
      local session = items[at].session
      delete_for_good(
        session,
        "Delete " .. (session.name or "this session") .. " and its worktree?"
      )
    elseif action == "sessions.restart" and id then
      command("restart", { session = id })
    elseif action == "sessions.fork" and id then
      -- v1 asked for the name first: `fork_active_session` prepared the spawn and
      -- opened its shared Session Name modal prefilled `<source>-fork`, so the
      -- fork was named before it existed and could be renamed on the spot. Forking
      -- silently was a v2 divergence, not a decision.
      --
      -- The naming UI lives in the creation float, so hand it the job through
      -- `store`, exactly as an irreversible change hands its question to
      -- `confirm`. This pane keeps owning what it knows (which session, and what
      -- the derived name is) and decides nothing about how it is asked.
      local source = items[at].session
      store.fork = {
        session = id,
        name = ((source and source.name) or "session") .. "-fork",
      }
    elseif action == "sessions.sync" and id then
      command("sync", { session = id })
    elseif action == "sessions.editor" and id then
      command("editor", { session = id })
    -- A move is computed over the RENDERED items and sent whole, so a root row
    -- drags its subtree and a group edge moves the group. The cursor FOLLOWS the
    -- session rather than the row index, since the order it was pressed at lands
    -- a frame or two later.
    elseif action == "sessions.move_down" and id then
      local moved = order.move_block(items, at, true)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.move_up" and id then
      local moved = order.move_block(items, at, false)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.sort" then
      cursor:follow(id)
      persist_order(order.sorted_within_groups(items))
    else
      return false
    end
    return true
  end,
}
