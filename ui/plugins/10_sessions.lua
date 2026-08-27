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
-- WHY THE BORDER IS DRAWN BY HAND. `Frame.title` is a plain, unstyled,
-- left-aligned string, so a frame cannot express any of the three things v1 puts
-- on its border: the focused title *badge* (inverted fg on an accent
-- background), the right-aligned per-session status dots, or the scroll counts
-- overlaid on the border cells. Drawing a header line inside the frame would
-- cost a content row and so would not be parity. Composing the border out of
-- `text` nodes costs nothing — the border rows are the rows the frame would have
-- occupied — and stays inside the four-kind vocabulary.

local chrome = require("lib.chrome")
local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local order = require("lib.order")
local panels = require("lib.panels")
local plugin_settings = require("lib.settings")
local scroll = require("lib.scroll")
local session_model = require("lib.session_model")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

-- ── Text helpers the contract needs and widgets.lua does not have ───────────

--- Append a raw-space span so a styled run covers the full width — how v1 makes
--- the selection background reach the right edge.
local function pad_spans(spans, width)
  local short = width - chrome.spans_len(spans)
  if short > 0 then
    spans[#spans + 1] = { text = string.rep(" ", short) }
  end
  return spans
end

--- ratatui's `Style::patch` over every span: the overlay wins for the fields it
--- sets, the span keeps the rest.
---
--- `keep_fg` names spans whose COLOUR is a signal of its own — the characters a
--- search query matched. They still take everything else the overlay sets, so the
--- selection bar paints through them (a bar with a gap in it is not a bar), but
--- their foreground survives it. v1 layered the same two the same way round:
--- `highlight_style` was built ON TOP of the row's base style
--- (`src/ui/highlight.rs`), so an accent match stayed accent on the selected row.
--- Patching over it left the match wearing the selection's own colour with only
--- its underline showing — on the one row the strip was pointing at, since
--- previewing a result moves this list's cursor onto it.
local function patch_spans(spans, style, keep_fg)
  for index, span in ipairs(spans) do
    local merged = {}
    for key, value in pairs(span.style or {}) do
      merged[key] = value
    end
    for key, value in pairs(style) do
      if not (keep_fg and keep_fg[index] and key == "fg") then
        merged[key] = value
      end
    end
    span.style = merged
  end
  return spans
end

-- ── The model, the border chrome and the ordering algebra live in lib/ ──────
--
-- `lib.session_model` builds the item list (one selectable unit per row, with
-- the group header glued to its group's first session), `lib.chrome` holds the
-- cell primitives and focus styles the hand-drawn border is composed from, and
-- `lib.order` is the move/sort algebra over the rendered items. All three are
-- pure over what this pane hands them; everything about how a row LOOKS stays
-- here.

-- ── Turning a model item into lines ────────────────────────────────────────

--- `── label ────────`, muted, full bleed. The header never reflects selection:
--- highlighting belongs to the session rows alone.
local function header_line(label, inner_width)
  local text = "── " .. label .. " "
  local used = widgets.len(text)
  if inner_width > used then
    text = text .. string.rep("─", inner_width - used)
  end
  return { { text = text, style = { fg = theme.muted } } }
end

local function status_glyph(status, elapsed)
  local spec = theme.status(status)
  if status == "working" then
    return theme.spinner_frame(elapsed), spec.color
  end
  return spec.glyph, spec.color
end

--- The status text that follows the name. v1 shows the agent's notification (or
--- the word "Blocked") for a blocked row, and the OSC activity title otherwise;
--- a row with neither carries no text, because the coloured dot already says
--- what state it is in.
---
--- Both come off the agent's own terminal — the activity line is its OSC window
--- title, the notification its OSC 9/777 message — so they are published from
--- the live pane rather than the database.
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
    if activity ~= "" then
      return activity
    end
  end
  return nil
end

--- Append the trailing status, budgeted against the width actually available.
--- Dropped rather than overflowed, exactly as v1 drops it.
local SEPARATOR = "  "
local MIN_WIDTH = 4

local function push_status(spans, text, style, inner_width)
  if not text or text == "" then
    return
  end
  local used = chrome.spans_len(spans) + widgets.len(SEPARATOR)
  local avail = math.max(0, inner_width - used)
  if avail >= MIN_WIDTH then
    spans[#spans + 1] = { text = SEPARATOR }
    spans[#spans + 1] = { text = widgets.truncate_hard(text, avail), style = style }
  end
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

local function session_line(item, inner_width, elapsed, is_selected, work, search)
  local session = item.session
  local glyph, glyph_color = status_glyph(session.status, elapsed)
  local status_style = { fg = glyph_color }
  -- A blocked row's text is an attention message, so it keeps the dot's colour;
  -- plain activity is muted, leaving the name the row's visual anchor. v1 draws
  -- the same split.
  local trailing = agent_status_text(session)
  local trailing_style = status_style
  if session.status ~= "blocked" then
    trailing_style = { fg = theme.muted }
  end

  -- Work already accepted but not yet in the snapshot is the more recent truth,
  -- so it takes the dot's place. v1 has no equivalent for a live row; the
  -- geometry is v1's, the signal is v2's.
  if work then
    if work.phase == "failed" then
      glyph, status_style = "✗", { fg = theme.role("status_error") }
      trailing = work.error and ("failed: " .. work.error) or "failed"
      trailing_style = { fg = theme.role("status_error") }
    else
      glyph, status_style = "◌", { fg = theme.muted }
      trailing, trailing_style = work.kind, { fg = theme.muted }
    end
  end

  local spans = { { text = " " .. glyph .. " ", style = status_style } }

  -- Nesting prefix: a tree mark for a child inside the group, a lone mark for
  -- one whose parent renders elsewhere in the list.
  if item.depth > 0 then
    spans[#spans + 1] = {
      text = string.rep("  ", item.depth - 1) .. "└ ",
      style = { fg = theme.muted },
    }
  elseif item.cross_group then
    spans[#spans + 1] = { text = "↳ ", style = { fg = theme.muted } }
  end

  -- An agent running on another machine, then a session that owns a worktree.
  if session.host then
    spans[#spans + 1] = { text = "⇅ ", style = { fg = theme.accent } }
  end
  if (session.worktrees or 0) > 0 then
    spans[#spans + 1] = { text = "⑂ ", style = { fg = theme.branch } }
  end

  -- Never truncated: the name is the row's anchor, and overflow clips.
  local hits = name_hits(session, search)
  local name_style = is_selected and { fg = theme.role("selection_fg"), bold = true }
    or { fg = theme.text }
  -- Which spans the search highlight owns, by position in `spans`. The overlays
  -- below are told, so the selection bar cannot repaint a match — see
  -- `patch_spans`. Identity on the style table is what marks one: `fuzzy.spans`
  -- hands back the very table it was given for a matched run.
  local matched_spans = nil
  if hits then
    local hit_style = { fg = theme.accent, bold = true, underline = true }
    matched_spans = {}
    for _, span in ipairs(fuzzy.spans(session.name or "?", hits, name_style, hit_style)) do
      spans[#spans + 1] = span
      if span.style == hit_style then
        matched_spans[#spans] = true
      end
    end
  else
    spans[#spans + 1] = { text = session.name or "?", style = name_style }
  end

  push_status(spans, trailing, trailing_style, inner_width)

  -- A row nothing matched is dimmed rather than hidden: v1 keeps every row on
  -- screen and lets the contrast do the filtering, so the list never jumps
  -- around under a cursor you are still moving.
  if search and hits == nil then
    patch_spans(spans, { fg = theme.muted, bold = false, underline = false })
  end

  -- The selection bar is painted here rather than by a list-wide highlight,
  -- which would bleed onto the group header glued above a group's first row.
  if is_selected then
    pad_spans(spans, inner_width)
    patch_spans(spans, {
      bg = theme.role("selection_bg"),
      fg = theme.role("selection_fg"),
      bold = true,
    }, matched_spans)
  elseif hover.id(session.id) then
    -- v1's row hover: a subtle band marking what a click would hit. Only the
    -- BACKGROUND is tinted — each cell keeps its own fg, so the status dot and
    -- the branch colour survive being hovered. A button gets the stronger
    -- accent fill instead (see the agent pane's chips); a row is not a button.
    --
    -- Skipped on the selected row because v1 tints it to the colour it already
    -- has, which is no change at all.
    pad_spans(spans, inner_width)
    patch_spans(spans, { bg = theme.role("selection_bg") })
  end

  return spans
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

local function pending_line(command, inner_width, elapsed)
  local failed = command.phase == "failed"
  local glyph, glyph_style
  if failed then
    glyph, glyph_style = "✗", { fg = theme.role("status_error") }
  else
    -- A spinner only while something is actually running.
    glyph, glyph_style = theme.spinner_frame(elapsed), { fg = theme.warn }
  end

  local label = command.subject or "new session"
  local phase
  if failed then
    phase = command.error and ("failed: " .. command.error) or "failed"
  else
    phase = PHASE_LABEL[command.phase] or "creating…"
  end

  local spans = {
    { text = " " .. glyph .. " ", style = glyph_style },
    { text = label, style = { fg = theme.secondary } },
  }
  -- Drop the phase rather than overflow a narrow panel.
  local used = 3 + widgets.len(label)
  if inner_width > used + widgets.len(phase) + 2 then
    spans[#spans + 1] = { text = "  " .. phase, style = { fg = theme.muted } }
  end
  return spans
end

--- Move the cursor by `step`, skipping items that select nothing.
local function move(items, from, step)
  local count = #items
  if count == 0 then
    return 1
  end
  local at = from
  for _ = 1, count do
    at = (at - 1 + step) % count + 1
    if items[at].target then
      return at
    end
  end
  return from
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
  if (git.ahead or 0) > 0 then
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

--- The chord bound to opening the creation flow, if anything is.
---
--- Read from the registry rather than hardcoded, because it is rebindable and
--- because the flow is removable: the empty state must not name a key that
--- resolves to nothing.
local function new_session_chord()
  for _, binding in ipairs((thurbox and thurbox.registry and thurbox.registry.keys) or {}) do
    if binding.action == "new_session.open" then
      return binding.key
    end
  end
  return nil
end

--- Persist a rendered order. Header ownership is a *rendering* property of the
--- first row in a group, so it is left to the next build rather than carried.
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
  --- Declared as data, so the settings modal renders a row for it without
  --- knowing what a repo group is. Read back through `lib.settings`.
  settings = {
    {
      id = "group_by_repo",
      desc = "Group sessions under a repo header",
      default = true,
    },
  },

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
    { key = "d", action = "sessions.delete", desc = "delete session", group = "Sessions" },
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
    local level = ctx.focused and "focused" or "active"
    local frame_style = chrome.border_style(level)
    if width < 2 or height < 2 then
      return { type = "text", text = "" }
    end
    local inner_width = width - 2
    local inner_height = height - 2

    local list = sessions()
    local items = session_model.build(list)
    local busy = session_model.pending()
    -- The live query, read once per render and compiled once: `session_line`
    -- runs per visible row, and each used to re-read the store and re-split
    -- the query per field.
    local query = search_query()
    local search = query and { text = query, needle = fuzzy.compile(query) } or nil

    -- Keep the cursor on the session it was on, not on a row number.
    --
    -- A reorder is a command: it lands a frame or two later, and the row that
    -- was under the cursor has moved by then. Following the id instead means
    -- holding J walks a session down the list, which is what you meant.
    local cursor = state.cursor or 1
    -- An outside request to select a session: a clicked OS notification, or
    -- `thurbox-cli session focus`. Consumed here and cleared, because the cursor
    -- is republished from this pane every frame — anything that merely wrote
    -- `store.selected` would be overwritten a frame later.
    if store.focus_session then
      state.follow = store.focus_session
      store.focus_session = nil
    end
    -- Another PANE may steer the selection by writing `store.selected` — the
    -- search strip jumping to a result, a task opening the session it spawned.
    -- Publishing the cursor every frame would undo that write a frame later, so a
    -- value this pane did not publish is read as a request and followed. v1's
    -- panes call `App::select_session` for the same reason.
    local steered = store.selected
    if steered and steered ~= state.published then
      state.follow = steered
    end
    if state.follow then
      for index, item in ipairs(items) do
        if item.target == state.follow then
          cursor = index
          break
        end
      end
    end
    cursor = widgets.clamp(cursor, #items)
    if #items > 0 and not items[cursor].target then
      cursor = move(items, cursor, 1)
    end
    state.cursor = cursor
    -- Publish the selection so the agent pane knows what to show, remembering
    -- what was published so an outside write can be told from our own echo.
    local target = items[cursor] and items[cursor].target or nil
    store.selected = target
    state.published = target

    -- One dot per session on the top border, in render order, each in its own
    -- status colour. Suppressed entirely when there are no sessions.
    local dots = {}
    for _, item in ipairs(items) do
      if item.kind == "session" then
        local glyph, color = status_glyph(item.session.status, ctx.elapsed)
        dots[#dots + 1] = { text = glyph, style = { fg = color } }
      end
    end

    -- One edge node shared by every row this render: the `│` cells are
    -- identical within a frame, and building two four-deep tables per row was
    -- pure churn (the agent pane's `edge` upvalue is the same pattern).
    local edge = { type = "text", len = 1, text = { { { text = "│", style = frame_style } } } }
    local function row(spans, id, class)
      return {
        type = "box",
        axis = "horizontal",
        len = 1,
        children = {
          edge,
          {
            type = "text",
            fill = 1,
            text = { spans },
            id = id,
            class = class,
            -- Decoration (the search plugin) finds rows by this role, and only
            -- the content is inside it — so a highlight can never repaint the
            -- border cells.
            role = id and "row" or nil,
          },
          edge,
        },
      }
    end

    local lines, above, below = {}, 0, 0

    if #items == 0 then
      -- v1's placeholder: a blank line, then two centred muted lines.
      local function centred(text)
        local pad = math.max(0, math.floor((inner_width - widgets.len(text)) / 2))
        return { { text = string.rep(" ", pad) .. text, style = { fg = theme.muted } } }
      end
      -- `{ spans = … }`, like every other entry: the row builder below reads
      -- `line.spans`, so a bare span list here renders as a blank row — which is
      -- exactly what the placeholder did until a test looked at it.
      lines[1] = { spans = {} }
      lines[2] = { spans = centred("No sessions yet") }
      -- v1's second line names the chord that creates one, and it is only shown
      -- when something actually answers it: the chord is looked up in the
      -- registry rather than written here, so a rebind — or the flow being
      -- removed — cannot leave the empty state advertising a key that does
      -- nothing.
      local chord = new_session_chord()
      if chord then
        lines[3] = { spans = centred("Press " .. chord .. " to create one") }
      end
    else
      local heights = {}
      for index, item in ipairs(items) do
        heights[index] = item.header and 2 or 1
      end

      local first, visible =
        scroll.window_variable(heights, state.offset or 0, cursor, inner_height)
      state.offset = first - 1
      above = first - 1
      below = #items - (first - 1 + visible)

      for index = first, #items do
        local item = items[index]
        if #lines >= inner_height then
          break
        end
        if item.header then
          lines[#lines + 1] = { spans = header_line(item.header, inner_width) }
        end
        if #lines < inner_height then
          if item.kind == "pending" then
            lines[#lines + 1] = {
              spans = pending_line(item.command, inner_width, ctx.elapsed),
              class = "pending-row",
            }
          else
            lines[#lines + 1] = {
              spans = session_line(
                item,
                inner_width,
                ctx.elapsed,
                index == cursor,
                busy[item.session.id],
                search
              ),
              id = item.session.id,
              class = "session-row",
            }
          end
        end
      end
    end

    local children = {}

    -- Top border: the title badge at the left, the dot strip right-aligned, and
    -- the "items above" count painted over it — the same layering v1 gets from
    -- a right-aligned title plus a paragraph drawn on the border cells.
    local top = chrome.new_cells(width, "─", frame_style)
    top[1] = { ch = "╭", style = frame_style }
    top[width] = { ch = "╮", style = frame_style }
    chrome.place_spans(top, 2, { { text = " Sessions ", style = chrome.title_style(level) } })
    if #dots > 0 then
      chrome.place_spans(top, width - chrome.spans_len(dots), dots)
    end
    if above > 0 then
      local text = "▲ " .. above .. " "
      chrome.place_text(top, width - widgets.len(text), text, { fg = theme.muted })
    end
    children[#children + 1] = { type = "text", len = 1, text = { chrome.cells_to_spans(top) } }

    for index = 1, inner_height do
      local line = lines[index]
      children[#children + 1] =
        row(line and line.spans or {}, line and line.id, line and line.class)
    end

    local bottom = chrome.new_cells(width, "─", frame_style)
    bottom[1] = { ch = "╰", style = frame_style }
    bottom[width] = { ch = "╯", style = frame_style }
    if below > 0 then
      local text = "▼ " .. below .. " "
      chrome.place_text(bottom, width - widgets.len(text), text, { fg = theme.muted })
    end
    children[#children + 1] = { type = "text", len = 1, text = { chrome.cells_to_spans(bottom) } }

    return { type = "box", children = children }
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
    local index = widgets.index_of(items, hit.id, "target")
    if not index then
      return false
    end
    state.follow = nil
    state.cursor = index
    store.selected = hit.id
    state.published = hit.id
    return true
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
        return false
      end
      command("restore", { session = state.deleted })
      state.deleted = nil
      return true
    end

    local list = sessions()
    local items = session_model.build(list)
    if #items == 0 then
      return false
    end
    local at = widgets.clamp(state.cursor or 1, #items)
    local id = items[at].target or nil

    -- Moving the cursor republishes the selection here as well as in render,
    -- because Ctrl+J/K are global: with the column hidden (F9) or a terminal
    -- focused, this pane may not render again before the agent pane does.
    local function select_row(index)
      state.follow = nil
      state.cursor = index
      local target = items[index] and items[index].target or store.selected
      store.selected = target
      state.published = target
    end

    -- Actions, not chords. The kernel already resolved which key was pressed,
    -- so the capital-vs-shift encoding trap is its problem now, not ours.
    if action == "sessions.open" then
      -- The agent pane is what shows a session; focusing it is what "open" means
      -- here, exactly as v1's Enter moves focus to the terminal.
      if id then
        command("focus", { text = "agent" })
      end
    elseif action == "sessions.next" then
      select_row(move(items, at, 1))
    elseif action == "sessions.previous" then
      select_row(move(items, at, -1))
    elseif action == "sessions.first" then
      select_row(move(items, 0, 1))

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
    -- drags its subtree and a group edge moves the group. `state.follow` keeps
    -- the cursor on the session rather than the row index, since the order it
    -- was pressed at lands a frame or two later.
    elseif action == "sessions.move_down" and id then
      local moved = order.move_block(items, at, true)
      if moved then
        state.follow = id
        persist_order(moved)
      end
    elseif action == "sessions.move_up" and id then
      local moved = order.move_block(items, at, false)
      if moved then
        state.follow = id
        persist_order(moved)
      end
    elseif action == "sessions.sort" then
      state.follow = id
      persist_order(order.sorted_within_groups(items))
    else
      return false
    end
    return true
  end,
}
