-- The session list.
--
-- In v1 this was `ui::project_list` — 2,212 lines of Rust, welded to a 616-method
-- `App` struct. Here it is a file you can edit while thurbox is running.
--
-- It is an ORDINARY plugin. The kernel has no session-list concept: it hands
-- over a snapshot and this decides everything about how the list looks.
--
-- Session rows retain v1's nesting marks, selection bar, status strip and
-- scroll indicators. Host and repo rows provide fold handles above them.
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
-- drop a trailing status, what a repo row says — while the window
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
-- `lib.session_model` builds the item list (one selectable unit per row), `lib.ui` is the
-- component layer — the panel, the list, the cursor and the row builder, and
-- with them the window arithmetic, the selection bar and the focus border this
-- pane used to spell itself — and `lib.order` is the move/sort algebra over the
-- rendered items. All three are pure over what this pane hands them; everything
-- about how a row LOOKS stays here.

-- ── Turning a model item into lines ────────────────────────────────────────

--- What the agent's own terminal title says it is doing, or nil.
---
--- The title the agent sets on its own terminal (OSC 0/2), which thurbox reads
--- off the pane and publishes as `activity`. It is not tmux's `window_name`,
--- which thurbox does not query. Drop the `✳`/`*` activity decorations
--- explicitly: Lua's `%w` is ASCII-only and would strip non-Latin words too.
local function current_action(session)
  local activity = session.activity
  activity = activity and activity:match("^%s*(.-)%s*$")
  if not activity or activity == "" then
    return nil
  end
  local stripped = activity:match("^✳%s*(.*)$") or activity:match("^%*%s*(.*)$") or activity
  if stripped == "" then
    return nil
  end
  return stripped
end

--- The status that follows the title, in two halves: `reason`, which says why
--- the dot looks the way it does and is kept at every width, and `action`, the
--- agent's current action.
---
--- v1 shows the agent's notification (or the word "Blocked") for a blocked row,
--- and the OSC activity title otherwise; a row with neither carries no text,
--- because the coloured dot already says what state it is in.
---
--- v2 adds the row nothing has reported for. Its dot says only that no status
--- arrived, which is honest but not diagnosable, so the text names what thurbox
--- does know: the agent found in the pane, or why the silence means nothing.
--- Naming the agent never displaces what it said.
local function agent_status(session)
  -- Nothing the agent said can be current on a host we cannot reach, so the
  -- row says why instead of showing a last message as if it were live.
  if session.status == "unreachable" then
    if session.remote_host then
      return "host " .. session.remote_host .. " unreachable", nil
    end
    return "unreachable", nil
  end
  if session.status == "blocked" then
    return session.notification or "Blocked", nil
  end
  local action = current_action(session)
  -- An agent thurbox did not launch: the row is labelled with whatever the
  -- driver asked for (`zsh`), and the agent actually in front of the user is
  -- named here. It says WHICH agent, never what it is doing — the dot already
  -- says that nothing has reported.
  local detected = session.detected_agent
  if detected then
    if action then
      return detected, action
    end
    return detected .. " · no status reported", nil
  end
  if action then
    return nil, action
  end
  if session.status == "uncovered" then
    return "no status hooks", nil
  end
  if session.status == "unreported" then
    return "no status reported", nil
  end
  -- An agent is demonstrably in the pane and could not be named: several
  -- registered profiles share its executable, so `ps` sees the command and not
  -- the profile. The row says what is actually known rather than falling
  -- silent, and it never guesses which profile.
  if session.status == "running" then
    return "agent running · no status reported", nil
  end
  return nil, nil
end

--- The multiplexer that is not installed, as short lines, or nil when it is.
---
--- Already an answer: the kernel probes on its own schedule behind a TTL, so
--- this is a table lookup on the render path rather than a `which`.
---
--- Deliberately short, and deliberately not the advice itself: this is drawn in
--- a sidebar around twenty-six cells wide, where the full sentence is truncated
--- to its first clause and says less than nothing. The command it names prints
--- the whole thing, with the search path — and the create-session flow, which
--- has the width for it, states the advice in full.
local function missing_multiplexer()
  local mux = (thurbox and thurbox.preflight and thurbox.preflight.mux) or nil
  if not mux or mux.presence ~= "missing" then
    return nil
  end
  return { "⚠ " .. mux.binary .. " is not installed", "run: thurbox-cli doctor" }
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
--- all. `false` means it matched on something else — its terminal text, or a
--- field other than the name — so the row stays lit but its name carries no
--- marks.
---
--- Whether a row matched at all is the search strip's answer, not this pane's:
--- the strip publishes the sessions it found (`search.matches`), which is the
--- only way a session found by its terminal text can be lit here. Before the
--- strip has published — the frame a query is typed — the row's own fields
--- decide, through the same `lib.fuzzy` grammar.
---
--- `search` is the render-level `{ q, matches }` pair: the query is parsed once
--- per render rather than per row.
local function name_hits(session, search)
  if not search then
    return nil
  end
  if search.matches and not search.matches[session.id] then
    return nil
  end
  local name = fuzzy.match_fields(search.q, { { name = "name", text = session.name or "" } })
  if name then
    return name.positions
  end
  if search.matches then
    return false
  end
  -- `or ""` on every field, so a session with no branch still has its
  -- repository tested.
  local fields = {
    { name = "name", text = session.name or "" },
    { name = "agent", text = session.agent or "" },
    { name = "branch", text = session.branch or "" },
    { name = "repo", text = session.repo or "" },
  }
  return fuzzy.match_fields(search.q, fields) and false or nil
end

--- From this pane width up, a session row has room for the agent's current
--- action in an aligned column. Narrow panels keep it after the session name.
local WIDE = 100

--- Show the agent's terminal title instead of the thurbox name?
---
--- Off by default: the name is what you gave the session and is stable, while
--- the title is whatever the agent last set. A row whose agent set none keeps
--- its name.
local function show_window_title()
  return plugin_settings.enabled("sessions", "show_window_title", false)
end

--- The text a session row is anchored on, and whether it is the agent's title
--- rather than the name (a search hit on the name cannot be marked on it).
local function title_of(session, terminal_title)
  if terminal_title then
    local action = current_action(session)
    if action then
      return action, true
    end
  end
  return session.name or "?", false
end

--- A session on another machine while host rows are off: its top-level row
--- says so where host rows would have. Its children do not repeat it.
local function remote_mark(item)
  return item.host == nil and item.session.host ~= nil and item.depth == 0
end

--- What a folded parent says it holds.
local function folded_count(item)
  return item.collapsed and (" +" .. item.descendants) or nil
end

--- Columns a session row spends before its title: the indent, the fold slot,
--- the dot and the marks. Kept beside `session_line`, which spends them, so
--- the aligned action column is measured from the row as it is drawn.
local function head_width(item, layout)
  local used = layout.base + 2 * item.depth + 4
  if item.cross_group then
    used = used + 2
  end
  if remote_mark(item) then
    used = used + 2
  end
  return used
end

--- Where the action column starts on a wide panel: just past the longest
--- title on screen, capped so a single long one cannot push every action off
--- the row. nil on a narrow panel, which has no such column.
local function action_column(items, layout)
  if layout.width + 2 < WIDE then
    return nil
  end
  local widest = 0
  for _, item in ipairs(items) do
    if item.kind == "session" then
      local title = title_of(item.session, layout.terminal_title)
      local suffix = folded_count(item)
      widest = math.max(
        widest,
        head_width(item, layout) + widgets.len(title) + (suffix and widgets.len(suffix) or 0)
      )
    end
  end
  return math.min(widest, math.floor(layout.width * 0.6)) + 2
end

--- The spans of one session row.
---
--- The row's own `style` — the selection bar, the hover band, the tint of a
--- row waiting for you — is `ui.list`'s, not this pane's: see `row_style`.
--- What is still decided here is every colour a span asks for, and `tone` is
--- where a row says the bar speaks for all of them.
---
--- Nesting is indentation alone: two columns per level, with a fold slot in
--- front of the dot that a parent fills with its handle and every other row
--- leaves blank, so a parent's dot and its siblings' line up.
local function session_line(item, layout, elapsed, is_selected, work, search)
  local session = item.session
  local spec = ui.status(session.status, elapsed, ui.printing(session.id))
  local glyph, glyph_color = spec.glyph, spec.color
  local width = layout.width
  -- A blocked row's text is an attention message, so it keeps the dot's colour;
  -- everything else is muted, leaving the title the row's visual anchor. v1
  -- draws the same split.
  local reason, action = agent_status(session)
  local trailing_color = glyph_color
  if session.status ~= "blocked" then
    trailing_color = theme.muted
    -- What whoever drives the session said about it (`ui.note` meta, issue
    -- #1523) — "waiting for CI" — which the agent's own title cannot carry.
    -- First, because somebody set it on purpose; muted, like activity. A
    -- blocked row keeps its attention message to itself, and an unreachable
    -- one its reason: neither line can afford the width.
    local note = session.meta and session.meta["ui.note"]
    note = note and note:match("^%s*(.-)%s*$")
    if note and note ~= "" and session.status ~= "unreachable" then
      reason = reason and (note .. " · " .. reason) or note
    end
  end

  local title, from_terminal = title_of(session, layout.terminal_title)
  -- The action is shown once: as the title, or after the session name.
  if from_terminal then
    action = nil
  end
  local trailing = reason
  if action then
    trailing = trailing and (trailing .. " · " .. action) or action
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
  -- Matched or not is still the name's answer; where it matched is not
  -- something the agent's title can show.
  if from_terminal and hits then
    hits = false
  end

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

  row:gap(layout.base + 2 * item.depth)
  if item.descendants > 0 then
    -- Its own click target, so pressing the handle folds the parent and
    -- pressing anywhere else on the row selects it.
    row:button(
      item.collapsed and "▸ " or "▾ ",
      { fg = theme.accent },
      nil,
      "fold:" .. session.id
    )
  else
    row:gap(2)
  end
  row:add(glyph .. " ", { fg = glyph_color })
  -- A child whose parent renders in another group: indentation cannot show
  -- that link, so the mark does.
  if item.cross_group then
    row:add("↳ ", { fg = theme.muted })
  end
  if remote_mark(item) then
    row:add("@ ", { fg = theme.accent })
  end

  -- A row waiting for you is lit in its status colour; a parent is bold, as
  -- the head of what is under it.
  local title_style = { fg = theme.text, bold = item.descendants > 0 or nil }
  if session.status == "blocked" and not work then
    title_style = { fg = glyph_color, bold = true }
  end
  local suffix = folded_count(item)
  local suffix_width = suffix and widgets.len(suffix) or 0
  local title_end = (layout.column and trailing) and (layout.column - 2) or width
  title = widgets.truncate(title, math.max(1, title_end - row.used - suffix_width))

  -- A matched run is the one thing that keeps its colour on a selected row: it
  -- names a foreground, and the bar underneath supplies only what a span left
  -- unsaid. v1 layered the same two the same way round — `highlight_style` was
  -- built ON TOP of the row's base style (`src/ui/highlight.rs`) — and
  -- previewing a result moves this list's cursor onto the row, so the selected
  -- row is exactly the one whose marks would otherwise disappear.
  row:match(title, hits, title_style, {
    fg = theme.accent,
    bold = true,
    underline = true,
  })
  if suffix then
    row:add(suffix, { fg = theme.muted })
  end

  if layout.column and trailing then
    row:gap(layout.column - row.used)
    return row
      :add(widgets.truncate_hard(trailing, width - row.used), { fg = trailing_color })
      :spans_list()
  end
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

--- The tint behind a row, or nil. A host row is a band in the accent, so the
--- machines read as the list's sections; a session waiting for you is washed in
--- its status colour, so it can be found from across the room. `done` needs no
--- answer and is not lit.
local function row_style(item)
  if item.kind == "host" then
    return { bg = theme.tint(theme.accent, 0.12) or theme.role("modal_bg") }
  end
  if item.kind == "session" and item.session.status == "blocked" then
    local bg = theme.tint(theme.role("status_blocked"), 0.16)
    return { bg = bg or theme.role("modal_bg") }
  end
  return nil
end

local function host_line(item, width, selected)
  local counts = item.counts
  local total = counts.total == 1 and "1 session" or counts.total .. " sessions"
  local details = total
  if counts.working > 0 then
    details = details .. " · " .. counts.working .. " active"
  end
  local local_host = item.host:sub(1, 1) == "\0"
  local wsl = not local_host and counts.backend and counts.backend:match("^wsl:") ~= nil
  for _, host in ipairs(thurbox.hosts or {}) do
    if host.name == item.host then
      wsl = host.backend:match("^wsl:") ~= nil
      break
    end
  end
  local color = theme.accent
  local reach = "connected"
  local reach_glyph = ""
  if counts.unreachable == counts.total and counts.total > 0 then
    reach, reach_glyph, color = "unreachable", "⊘", theme.role("status_unreachable")
  elseif counts.failed > 0 and counts.total == 0 then
    reach, reach_glyph, color = "failed", "✗", theme.role("status_error")
  elseif counts.total == 0 then
    reach, reach_glyph, color = "connecting", "◌", theme.warn
  end
  local row = ui.row({ width = width, tone = selected and function()
    return nil
  end or nil })
  -- Every host's handle shares column 1, one column left of its repo rows and
  -- of the sessions it holds directly, three left of those under a repo row.
  -- The reach colour is the handle's; the mark and the name are one accent
  -- run, so `@` reads as part of the name.
  row:add((item.collapsed and " ▸ " or " ▾ ") .. reach_glyph, { fg = color, bold = true })
  local label = local_host and "⌂ local" or wsl and ("@ WSL " .. item.host) or ("@ " .. item.host)
  row:add(label, { fg = theme.accent, bold = true })
  if counts.attention > 0 then
    row:add("  !" .. counts.attention, { fg = theme.role("status_blocked"), bold = true })
  end
  local note = reach == "connected" and details or reach .. " · " .. details
  local short = reach == "connected" and total or reach .. " · " .. total
  -- The rule runs up to a right-aligned note, and is what marks a host, so the
  -- active count goes before it does. With no room for a rule at all, the
  -- short note trails the label as any row's does.
  for _, candidate in ipairs({ note, short }) do
    local rule = width - row.used - widgets.len(candidate) - 2
    if rule >= 1 then
      row:add(" " .. string.rep("─", rule) .. " ", { fg = theme.muted })
      row:add(candidate, { fg = theme.secondary })
      return row:spans_list()
    end
  end
  row:trailing(short, { fg = theme.secondary })
  return row:spans_list()
end

--- A repository row is a separator, not a level: a lighter dotted rule, one
--- column in from its host's handle, and its sessions are indented from the
--- host rather than from it. Folded, it says how many sessions it holds.
local function repo_line(item, width, selected)
  local row = ui.row({ width = width, tone = selected and function()
    return nil
  end or nil })
  row:gap(item.host and 2 or 1)
  row:add(item.collapsed and "▸ " or "▾ ", { fg = theme.accent })
  local count = nil
  if item.collapsed then
    count = item.count == 1 and "1 session" or item.count .. " sessions"
  end
  local reserve = count and widgets.len(count) + 3 or 2
  row:add(widgets.truncate(item.repo_label, math.max(1, width - row.used - reserve)), {
    fg = theme.accent,
  })
  local rule = width - row.used - (count and widgets.len(count) + 2 or 1)
  if rule >= 1 then
    row:add(" " .. string.rep("╌", rule) .. (count and " " or ""), { fg = theme.muted })
  end
  if count then
    row:add(count, { fg = theme.secondary })
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
  return plugin_settings.feature("soft_delete", false) == true
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
--- The worktree *directory* is deliberately not a reason to ask: a clean one
--- holds nothing its base does not. Force-delete also deletes a branch thurbox
--- created, which is why unpushed commits are a reason of their own above. The
--- directory is listed as context for a question already owed, never as the
--- cause.
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

--- Delete for good, always asking before a worktree is removed.
---
--- The risk lines still distinguish known-clean work from work that cannot
--- be recovered; the policy requires an intentional second step in both cases.
---
--- The question travels through `store`, so the confirm plugin needs to know
--- nothing about sessions.
local function delete_for_good(session, question)
  local lines = at_risk(session)
  store.confirm = {
    question = question,
    lines = lines or {},
    command = "delete",
    options = { session = session.id, force = true },
  }
end

--- This list's cursor, in the one spelling every handler here reads it with.
---
--- `target` is a model item's identity — a session id, or a host id for its
--- fold row. `publish` keeps that host id out of `store.selected`, which the
--- agent pane reads only as a session. `steer` is the `store` key
--- another pane writes to move this list; `request` is the one-shot
--- `focus_session` a clicked notification or `thurbox-cli session focus` leaves,
--- and it is read only by `render` because consuming it anywhere else would
--- spend it on a frame that is not being drawn.
local function selected_session(item)
  return item and item.kind == "session" and item.target or nil
end

local function first_session_index(items)
  for index, item in ipairs(items) do
    if item.kind == "session" then
      return index
    end
  end
  return 1
end

local CURSOR_OPTS = {
  id = "target",
  steer = "selected",
  publish = selected_session,
  initial = first_session_index,
}
local CURSOR_OPTS_WITH_REQUEST = {
  id = "target",
  steer = "selected",
  publish = selected_session,
  initial = first_session_index,
  request = "focus_session",
}

local function folds(setting)
  local saved = plugin_settings.get("sessions", setting, "")
  if type(saved) ~= "string" then
    saved = ""
  end
  if type(state[setting]) == "string" then
    if state[setting .. "_base"] == saved then
      return state[setting]
    end
    state[setting .. "_base"] = nil
    state[setting] = nil
  end
  return saved
end

local function folded_hosts()
  return folds("folded_hosts")
end

local function folded_repos()
  return folds("folded_repos")
end

local function escape_fold_key(name)
  return name:gsub("[^%w._-]", function(char)
    return string.format("%%%02X", string.byte(char))
  end)
end

local function is_folded(setting, name)
  local saved = ";" .. folds(setting) .. ";"
  return saved:find(";" .. escape_fold_key(name) .. ";", 1, true) ~= nil
end

local function save_folds(setting, value)
  state[setting .. "_base"] = plugin_settings.get("sessions", setting, "")
  state[setting] = value
  command("set", { text = "sessions." .. setting, value = value })
end

local function set_folded(setting, name, folded)
  local names = {}
  for escaped in folds(setting):gmatch("[^;]+") do
    local decoded = escaped:gsub("%%(%x%x)", function(hex)
      return string.char(tonumber(hex, 16))
    end)
    names[decoded] = true
  end
  names[name] = folded or nil
  local ordered = {}
  for key in pairs(names) do
    ordered[#ordered + 1] = escape_fold_key(key)
  end
  table.sort(ordered)
  save_folds(setting, table.concat(ordered, ";"))
end

local function host_is_folded(host)
  return is_folded("folded_hosts", host)
end

local function set_host_folded(host, folded)
  set_folded("folded_hosts", host, folded)
end

local function repo_is_folded(target)
  return is_folded("folded_repos", target)
end

local function set_repo_folded(target, folded)
  set_folded("folded_repos", target, folded)
end

local function parent_is_folded(id)
  return is_folded("folded_parents", id)
end

local function set_parent_folded(id, folded)
  set_folded("folded_parents", id, folded)
end

local function folded_parents()
  return folds("folded_parents")
end

local function model_items(searching)
  return session_model.build(
    sessions(),
    folded_hosts(),
    searching,
    true,
    folded_repos(),
    folded_parents()
  )
end

---@param reveal_selected? boolean
local function model(reveal_selected)
  -- An empty list leaves the cursor at row one. When its first session arrives,
  -- that row is now the local host handle; start on the session instead.
  local has_sessions = #sessions() > 0
  if has_sessions and not state["sessions.had_session"] then
    state["sessions.cursor"] = nil
    state["sessions.selected"] = nil
  end
  state["sessions.had_session"] = has_sessions

  -- A focus from another pane or the CLI must be able to reach a folded child.
  -- `ui.cursor` consumes the request after this model is built, so uncover its
  -- host first; the setting write also makes the reveal survive that frame.
  local searching = search_query() ~= nil
  local search_open = panels.shown("search") or searching
  local search_closed = state.search_was_open and not search_open
  state.search_was_open = search_open
  if search_closed then
    -- Search revealed hidden rows temporarily. Its selection is not a new
    -- focus request, so let the saved fold resume when the strip closes.
    state["sessions.follow"] = nil
  end
  local accepted = store["search.accepted_session"]
  store["search.accepted_session"] = nil
  local wanted = accepted or store.focus_session
  local selected = store.selected
  if not wanted and not search_open and not search_closed then
    wanted = state["sessions.follow"]
    if not wanted and selected ~= state["sessions.published"] then
      wanted = selected
    end
    -- A snapshot can hide the active row when its blocked state ends. Keep
    -- visible selections on the cached path instead of rebuilding an unfolded model.
    if not wanted and reveal_selected ~= false and selected then
      local items = model_items(searching)
      for _, item in ipairs(items) do
        if item.target == selected then
          return items
        end
      end
      wanted = selected
    end
  end
  if type(wanted) == "string" then
    local all = session_model.build(sessions(), "", false, true, "")
    for _, item in ipairs(all) do
      if item.kind == "session" and item.target == wanted and repo_is_folded(item.repo_target) then
        set_repo_folded(item.repo_target, false)
        break
      end
    end
    local parent_of, wanted_session = {}, nil
    for _, session in ipairs(sessions()) do
      parent_of[session.id] = session.parent
      if session.id == wanted then
        wanted_session = session
      end
    end
    if wanted_session then
      local host = wanted_session.host or "\0local"
      if host_is_folded(host) then
        set_host_folded(host, false)
      end
      -- A blocked row is never folded away, so reaching one must not undo a
      -- fold you chose. Stop at a repeated ancestor so a parent cycle cannot
      -- hang the render, while every ancestor of a deep valid tree is revealed.
      if wanted_session.status ~= "blocked" then
        local ancestor, seen = parent_of[wanted], { [wanted] = true }
        while ancestor and not seen[ancestor] do
          seen[ancestor] = true
          if parent_is_folded(ancestor) then
            set_parent_folded(ancestor, false)
          end
          ancestor = parent_of[ancestor]
        end
      end
    end
  end
  return model_items(searching)
end

--- Header ownership is a rendering property of the first row in a group, so
--- persist session ids and let the next build restore each header.
local function persist_order(items)
  local ids = {}
  for _, item in ipairs(items) do
    if item.kind == "session" and item.target then
      ids[#ids + 1] = item.target
    end
  end
  if #ids > 0 then
    command("order", { list = ids })
  end
end

local function ordering_items()
  local items = session_model.build(sessions(), "", false, true, "")
  local ordered = {}
  local previous_host
  for _, item in ipairs(items) do
    if item.kind ~= "host" and item.kind ~= "repo" then
      if item.host ~= previous_host and item.header == nil then
        local copy = {}
        for key, value in pairs(item) do
          copy[key] = value
        end
        copy.header = item.host or "local"
        ordered[#ordered + 1] = copy
      else
        ordered[#ordered + 1] = item
      end
      previous_host = item.host
    end
  end
  return ordered
end

--- The right-press menu: the actions that target one session, in the order
--- the spec settled. Actions only -- each runs back through `on_action`, so an
--- entry and its chord cannot come to mean different things, and delete still
--- asks first when there is work to lose. Sort, the panel toggle and undo are
--- not here: none of them is about the session that was pressed.
local SESSION_MENU = {
  { label = "Open", action = "sessions.open" },
  { label = "Rename", action = "sessions.rename" },
  { label = "Fork", action = "sessions.fork" },
  { label = "Open in editor", action = "sessions.editor" },
  "sep",
  { label = "Restart", action = "sessions.restart" },
  { label = "Sync", action = "sessions.sync" },
  { label = "Move up", action = "sessions.move_up" },
  { label = "Move down", action = "sessions.move_down" },
  "sep",
  { label = "Delete", action = "sessions.delete" },
  { label = "Delete + worktree", action = "sessions.force_delete" },
}

--- Whether some plugin declares `action`, in `keys` or in `commands`. An
--- undeclared one would reach no `on_action` and close the menu doing nothing,
--- and an entry that does nothing is worse than no entry.
local function declared(action)
  local registry = thurbox.registry
  for _, list in ipairs({ registry and registry.keys, registry and registry.commands }) do
    for _, row in ipairs(list or {}) do
      if row.action == action then
        return true
      end
    end
  end
  return false
end

local function fold_menu_entries()
  local expanded, collapsed = false, false
  for _, item in ipairs(session_model.build(sessions(), "", false, true, "")) do
    local parent = item.kind == "session" and item.descendants > 0
    if item.kind == "host" or item.kind == "repo" or parent then
      local folded = item.kind == "host" and host_is_folded(item.host)
        or item.kind == "repo" and repo_is_folded(item.target)
        or parent and parent_is_folded(item.target)
      collapsed = collapsed or folded
      expanded = expanded or not folded
    end
  end
  local entries = {}
  if expanded then
    entries[#entries + 1] = { label = "Collapse all", action = "sessions.collapse_all" }
  end
  if collapsed then
    entries[#entries + 1] = { label = "Expand all", action = "sessions.expand_all" }
  end
  return entries
end

--- A row's menu: session and fold actions, then what other plugins left in
--- `store["sessions.menu_extra"]` -- a table from each contributor's own name
--- to its list of entries and "sep" rules, so two contributors never overwrite
--- each other. Contributors are taken in name order, each after a rule; an
--- entry whose action nothing declares is dropped, and so is a rule it leaves
--- with nothing to separate. A label that is not a string is dropped. Built when the menu opens, so it follows a plugin
--- that was added, removed or rebound since.
local function row_menu()
  ---@type (table|string)[]
  local base = {}
  for _, entry in ipairs(SESSION_MENU) do
    base[#base + 1] = entry
  end
  local fold_entries = fold_menu_entries()
  if #fold_entries > 0 then
    base[#base + 1] = "sep"
    for _, entry in ipairs(fold_entries) do
      base[#base + 1] = entry
    end
  end
  local extra = store["sessions.menu_extra"]
  if type(extra) ~= "table" then
    return base
  end
  local owners = {}
  for owner, entries in pairs(extra) do
    if type(owner) == "string" and type(entries) == "table" then
      owners[#owners + 1] = owner
    end
  end
  if #owners == 0 then
    return base
  end
  table.sort(owners)
  ---@type (table|string)[]
  local menu = {}
  for i, entry in ipairs(base) do
    menu[i] = entry
  end
  for _, owner in ipairs(owners) do
    local rule = true
    for _, entry in ipairs(extra[owner]) do
      if entry == "sep" then
        rule = true
      elseif
        type(entry) == "table"
        and type(entry.action) == "string"
        and declared(entry.action)
      then
        if rule then
          menu[#menu + 1] = "sep"
          rule = false
        end
        -- A label that is not text would stop the menu float from drawing at
        -- all, this pane's own entries included; without one it draws the
        -- action's name.
        local label = type(entry.label) == "string" and entry.label or nil
        menu[#menu + 1] = { label = label, action = entry.action }
      end
    end
  end
  return menu
end

--- The menu a right press on empty space opens: it is about no session, and a
--- group row is a fold handle instead. Built when it opens, because two of its
--- entries are offered only when they would do something: an entry that does
--- nothing is worse than no entry.
local function pane_menu(items)
  -- Entries and "sep" rules in one list: declared, or luals infers a list of
  -- tables from the first two and rejects the rules.
  ---@type (table|string)[]
  local menu = {
    { label = "New session", action = "new_session.open" },
    { label = "Restore deleted…", action = "restore.open" },
  }
  -- A creation in flight draws a placeholder row with no session behind it,
  -- so it is not something to sort: count the rows that are sessions.
  local live = false
  for _, item in ipairs(items) do
    if item.session then
      live = true
      break
    end
  end
  local middle = {}
  if live then
    middle[#middle + 1] = { label = "Sort by name", action = "sessions.sort" }
  end
  for _, entry in ipairs(fold_menu_entries()) do
    middle[#middle + 1] = entry
  end
  if store["sessions.deleted"] then
    middle[#middle + 1] = { label = "Undo delete", action = "sessions.undo" }
  end
  if #middle > 0 then
    menu[#menu + 1] = "sep"
    for _, entry in ipairs(middle) do
      menu[#menu + 1] = entry
    end
  end
  menu[#menu + 1] = "sep"
  menu[#menu + 1] = { label = "Hide panel", action = "sessions.toggle_panel" }
  return menu
end

local pane
pane = {
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

  ui_state = function()
    local _, folded_count = folded_hosts():gsub("[^;]+", "")
    local selected
    for _, item in ipairs(session_model.build(sessions(), "", false, true, "")) do
      if item.target == state["sessions.selected"] then
        selected = item
        break
      end
    end
    local host = selected and selected.host
    local repo = selected and (selected.kind == "repo" and selected.target or selected.repo_target)
    return {
      selected_row = state["sessions.selected"],
      selected_host = host == "\0local" and "local" or host,
      host_is_local = host == "\0local",
      host_collapsed = host and host_is_folded(host) or false,
      folded_host_count = folded_count,
      repo_collapsed = repo and repo_is_folded(repo) or false,
    }
  end,

  actions = {
    {
      name = "sessions.collapse_host",
      desc = "fold group",
      args = { { name = "host", kind = "string" } },
    },
    {
      name = "sessions.expand_host",
      desc = "unfold group",
      args = { { name = "host", kind = "string" } },
    },
    {
      name = "sessions.toggle_host",
      desc = "toggle host or repo fold",
      args = { { name = "host", kind = "string" } },
    },
  },

  -- Declared as DATA, not just handled. That is what lets the kernel list these
  -- in help, detect a clash with another plugin, and let you rebind them —
  -- none of which it could do if they only existed inside on_key.
  --- Declared as data, so the settings modal renders a row for each without
  --- knowing what a repo group is or what creating a session does. Read back
  --- through `lib.settings`.
  settings = {
    {
      id = "group_by_repo",
      desc = "Group sessions under foldable repo rows",
      default = true,
    },
    -- The other grouping axis, and a separate row because the two are
    -- independent: every combination of the pair renders, from one flat list
    -- to host rows with repo rows inside. On by default, including a
    -- local-only list so local work can be folded.
    {
      id = "group_by_host",
      desc = "Group sessions under foldable host rows",
      default = true,
    },
    -- Off by default: the name is the one you chose and stays put, while the
    -- title is whatever the agent last set.
    {
      id = "show_window_title",
      desc = "Show the agent's terminal title instead of the session name",
      default = false,
    },
    {
      id = "focus_new_session",
      desc = "Select and open a session when you create or fork it",
      default = false,
    },
    {
      id = "folded_hosts",
      desc = "Folded hosts (managed by the session list)",
      default = "",
      list = true,
    },
    {
      id = "folded_repos",
      desc = "Folded repos (managed by the session list)",
      default = "",
      list = true,
    },
    {
      id = "folded_parents",
      desc = "Folded parent sessions (managed by the session list)",
      default = "",
      list = true,
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
    { key = "home", action = "sessions.first", desc = "first row", group = "Navigation" },
    { key = "end", action = "sessions.last", desc = "last row", group = "Navigation" },
    { key = "G", action = "sessions.last", desc = "last row", group = "Navigation" },
    { key = "pageup", action = "sessions.page_up", desc = "previous page", group = "Navigation" },
    { key = "pagedown", action = "sessions.page_down", desc = "next page", group = "Navigation" },
    { key = "[", action = "sessions.previous_host", desc = "previous host", group = "Navigation" },
    { key = "]", action = "sessions.next_host", desc = "next host", group = "Navigation" },
    {
      key = "n",
      action = "sessions.next_attention",
      desc = "next session needing attention",
      group = "Navigation",
    },
    { key = "H", action = "sessions.collapse_all", desc = "fold all groups", group = "Navigation" },
    { key = "L", action = "sessions.expand_all", desc = "unfold all groups", group = "Navigation" },
    { key = "h", action = "sessions.collapse_host", desc = "fold group", group = "Navigation" },
    { key = "l", action = "sessions.expand_host", desc = "unfold group", group = "Navigation" },
    {
      key = "left",
      action = "sessions.parent_host",
      desc = "fold group or select its parent",
      group = "Navigation",
    },
    {
      key = "right",
      action = "sessions.first_child",
      desc = "unfold group or select its first child",
      group = "Navigation",
    },
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
    -- Readline's end-of-line, so passthrough like the chords above. Not `f2`,
    -- the other conventional rename key: the info panel maintained out of tree
    -- binds it, and a bundled claim would take it from that pane.
    {
      key = "ctrl+e",
      action = "sessions.rename",
      desc = "rename session",
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

    local items = model()
    local busy = session_model.pending()
    -- The live query, read once per render and parsed once: `session_line`
    -- runs per visible row, and each used to re-read the store and re-split
    -- the query per field.
    local query = search_query()
    local search = nil
    if query then
      local matches = nil
      if type(store["search.matches"]) == "string" then
        matches = {}
        for id in store["search.matches"]:gmatch("%S+") do
          matches[id] = true
        end
      end
      search = { q = fuzzy.query(query), matches = matches }
    end

    -- The cursor is re-derived from the SESSION it was on rather than restored
    -- as a row number, is steered by another pane writing `store.selected`, and
    -- answers a focus request from outside the interface — all three written
    -- once in `ui.cursor` and shared with the two handlers below.
    --
    -- The first clause used to be this comment's claim rather than
    -- `ui.cursor`'s behaviour: only a follow, a foreign steer or a focus request
    -- remapped the index onto an id, so a plain rebuild kept the number and a
    -- session opening or closing above the cursor slid a different session under
    -- the highlight — and into the agent pane, which draws `store.selected`
    -- (issue #1211). A comment stating the intent instead of the code is what
    -- let that survive: the bug was reported, not noticed here.
    local cursor = ui.cursor("sessions", items, CURSOR_OPTS_WITH_REQUEST)
    state["sessions.page_size"] = math.max(1, height - 2)

    -- One step in per group row above a session: past a repo row, in from a
    -- host row alone to where a repo row would sit, or at the edge of a list
    -- with neither (issue #1605).
    local base = 1
    for _, item in ipairs(items) do
      if item.kind == "repo" then
        base = 3
        break
      elseif item.kind == "host" then
        base = 2
      end
    end
    local layout = {
      width = inner_width,
      base = base,
      terminal_title = show_window_title(),
    }
    layout.column = action_column(items, layout)

    return ui.panel({
      title = "Sessions",
      focused = ctx.focused,
      -- No status strip on the border: every row carries its own dot, and the
      -- strip repeated them without saying which was which. The scroll counts
      -- still ride the border, from the list's own hidden-row counts.
      body = ui.list({
        items = items,
        cursor = cursor,
        width = inner_width,
        height = height - 2,
        on_overflow = "border",
        -- The pane is a column of its own: it holds its rows apart from the
        -- bottom border however few of them there are.
        pad = true,
        style_of = row_style,
        class_of = function(item)
          if item.kind == "host" then
            return "host-row"
          end
          if item.kind == "repo" then
            return "repo-row"
          end
          return item.kind == "pending" and "pending-row" or "session-row"
        end,
        row = function(item, selected)
          if item.kind == "host" then
            return host_line(item, inner_width, selected)
          end
          if item.kind == "repo" then
            return repo_line(item, inner_width, selected)
          end
          if item.kind == "pending" then
            return pending_line(item.command, inner_width, ctx.elapsed)
          end
          return session_line(item, layout, ctx.elapsed, selected, busy[item.session.id], search)
        end,
        -- v1's placeholder, and its second line names the chord that creates a
        -- session — shown only while something actually answers it, so a rebind
        -- or the flow being removed cannot leave this advertising a dead key.
        empty = ui.empty({
          title = "No sessions yet",
          width = inner_width,
          hint = "Press %s to create one",
          hint_action = "new_session.open",
          -- The empty list is the one screen a first run always reaches, and
          -- the multiplexer is what a session's window is made of: saying it is
          -- missing here is the difference between reading it now and finding
          -- out from a pane that died. Nil on any machine that has it.
          note = missing_multiplexer(),
          note_colour = theme.bad,
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

  -- A click on a row selects it — v1's `ClickAction::SelectSession`. The row
  -- carries the session id rather than an index, so a list that reordered
  -- between the paint and the press still selects the session you pointed at.
  --
  -- Selecting and opening are two gestures. A single click leaves the keyboard
  -- in this column (the kernel's click-focuses-the-pane rule), so Ctrl+D and
  -- the other list chords act on the row you just pointed at. A double-click
  -- is Enter (`sessions.open`): it also hands focus to the agent pane that
  -- shows the session. #1137 moved focus on every click, which made "click a
  -- session, press Ctrl+D" type Ctrl+D into the agent instead. The `focus`
  -- command is applied after the press resolves, so the agent wins.
  --
  -- Host and repo rows have ids and toggle their fold state on one click.
  on_click = function(hit)
    if not hit.id then
      return false
    end
    -- Keep a selection that remains visible. A folded-away child selects its
    -- parent, never whichever unrelated row took the child's place.
    local parent = hit.id:match("^fold:(.+)$")
    if parent then
      if hit.clicks ~= 2 then
        local cursor = ui.cursor("sessions", model(), CURSOR_OPTS)
        local selected = cursor:id()
        -- The click ends any earlier follow before the model can reveal it.
        cursor:select(cursor.index)
        set_parent_folded(parent, not parent_is_folded(parent))
        for _, item in ipairs(model(false)) do
          if item.target == selected then
            return true
          end
        end
        cursor:select_by_id(parent)
      end
      return true
    end
    local items = model()
    if hit.id:match("^host:") or hit.id:match("^repo:") then
      if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
        return false
      end
      -- The first press already toggled; treating its second as another
      -- toggle would make a double-click undo itself.
      if hit.clicks ~= 2 then
        pane.on_action("sessions.toggle_host")
      end
      return true
    end
    if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
      return false
    end
    if hit.clicks == 2 then
      command("focus", { text = "agent" })
    end
    return true
  end,

  -- A right press on a row selects it, as a left press would, and opens its
  -- menu where the press was. Selecting is what aims the entries: each runs an
  -- action on the selected session. Focus stays put -- the kernel's rule for a
  -- right press -- and the menu takes every key while it is up anyway. Entries
  -- other plugins contribute (`row_menu`) come last, and read the row from
  -- the action's session_id argument, not the selection. On empty space the
  -- press opens the pane's general menu. Host and repo rows are fold handles
  -- for either mouse button.
  on_context = function(hit)
    local items = model()
    -- The fold handle is part of its session's row for a menu.
    local parent = hit.id and hit.id:match("^fold:(.+)$")
    if parent then
      hit = { id = parent, screen_x = hit.screen_x, screen_y = hit.screen_y }
    end
    if hit.id and (hit.id:match("^host:") or hit.id:match("^repo:")) then
      if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
        return false
      end
      return pane.on_action("sessions.toggle_host")
    end
    if not hit.id then
      store.menu = { at = { x = hit.screen_x, y = hit.screen_y }, items = pane_menu(items) }
      return true
    end
    if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
      return false
    end
    store.menu = {
      at = { x = hit.screen_x, y = hit.screen_y },
      items = row_menu(),
      target = hit.id,
      target_argument = "session_id",
    }
    return true
  end,

  on_action = function(action, args)
    -- The two that own no row, handled before the "is there a session" guard:
    -- hiding the column and undoing a delete both work on an empty list.
    if action == "sessions.toggle_panel" then
      panels.toggle("sessions")
      return true
    elseif action == "sessions.undo" then
      -- v1's Ctrl+Z undoes the delete YOU just did — `App::undo_delete`
      -- restores its own `pending_delete` — rather than reaching for the most
      -- recently deleted row, which may belong to another instance.
      local deleted = store["sessions.deleted"]
      if not deleted then
        -- Said out loud rather than swallowed. Ctrl+Z is global: it fires
        -- from a focused terminal, and with the column hidden (F9) there is
        -- nothing on screen to tell "there was nothing to undo" from a chord
        -- that never arrived.
        command("message", { text = "nothing to undo" })
        return true
      end
      command("restore", { session = deleted })
      store["sessions.deleted"] = nil
      return true
    end

    local items = model()
    if #items == 0 then
      return false
    end
    -- The same cursor `render` builds, from the same state: moving it here
    -- republishes the selection as well, because Ctrl+J/K are global — with the
    -- column hidden (F9) or a terminal focused, this pane may not render again
    -- before the agent pane does.
    local cursor = ui.cursor("sessions", items, CURSOR_OPTS)

    local requested_host = type(args) == "table" and args.host or nil
    if requested_host then
      -- The local group's sentinel cannot be supplied on a command line.
      local host = requested_host == "" and "\0local" or requested_host
      if cursor:select_by_id("host:" .. host) == nil then
        command("message", { text = "that host has no group", level = "error" })
        return true
      end
    end

    -- An entry of the right-press menu is about the session it was opened on,
    -- not whatever row the cursor holds when the action lands: the cursor may
    -- have moved since, or that session gone and the cursor fallen back onto a
    -- neighbour -- which Delete + worktree must never reach in its place.
    local target = type(args) == "table" and args.session_id or nil
    local chosen = store["menu.chosen"]
    if type(chosen) == "table" and chosen.action == action then
      store["menu.chosen"] = nil
      target = target or chosen.target
    end
    if target then
      if cursor:select_by_id(target) == nil then
        command("message", { text = "that session is gone", level = "error" })
        return true
      end
    end

    local at = cursor.index
    local id = cursor:id()
    local selected = items[at]

    -- A file tree's arrows, one level at a time: host, then repo, then
    -- session. Either group level is absent while its axis is off, so a step
    -- out that finds no repo row goes on to the host row.
    local group = selected and (selected.kind == "host" or selected.kind == "repo")
    -- A parent session is a level of its own for the arrows, between its
    -- group and its children.
    local parent = selected and selected.kind == "session" and selected.descendants > 0
    if action == "sessions.parent_host" then
      if group and not selected.collapsed then
        return pane.on_action("sessions.collapse_host")
      elseif parent and not selected.collapsed then
        set_parent_folded(selected.target, true)
      elseif
        selected
        and selected.kind == "session"
        and selected.depth > 0
        and cursor:select_by_id(selected.visible_parent)
      then
        return true
      elseif selected and selected.kind ~= "host" then
        local repo = selected.kind ~= "repo" and selected.repo_target
        if not (repo and cursor:select_by_id(repo)) and selected.host then
          cursor:select_by_id("host:" .. selected.host)
        end
      end
      return true
    elseif action == "sessions.first_child" then
      if parent then
        if selected.collapsed then
          set_parent_folded(selected.target, false)
        elseif items[at + 1] and items[at + 1].depth == selected.depth + 1 then
          cursor:select(at + 1)
        end
        return true
      end
      if not group then
        return true
      end
      if selected.collapsed then
        return pane.on_action("sessions.expand_host")
      end
      -- Expanded, so the row below is its first child.
      local child = items[at + 1]
      if
        child
        and (
          selected.kind == "host" and child.host == selected.host
          or child.repo_target == selected.target
        )
      then
        cursor:select(at + 1)
      end
      return true
    end

    if action == "sessions.toggle_host" then
      if group then
        return pane.on_action(
          selected.collapsed and "sessions.expand_host" or "sessions.collapse_host"
        )
      end
      return false
    end

    if action == "sessions.collapse_all" or action == "sessions.expand_all" then
      local folding = action == "sessions.collapse_all"
      local hosts, repos, parents = {}, {}, {}
      local all = session_model.build(sessions(), "", false, true, "")
      if folding then
        for _, item in ipairs(all) do
          if item.kind == "host" then
            hosts[#hosts + 1] = escape_fold_key(item.host)
          elseif item.kind == "repo" then
            repos[#repos + 1] = escape_fold_key(item.target)
          elseif item.kind == "session" and item.descendants > 0 then
            parents[#parents + 1] = escape_fold_key(item.target)
          end
        end
        cursor:select(1)
      end
      table.sort(hosts)
      table.sort(repos)
      table.sort(parents)
      save_folds("folded_hosts", table.concat(hosts, ";"))
      save_folds("folded_repos", table.concat(repos, ";"))
      save_folds("folded_parents", table.concat(parents, ";"))
      return true
    end

    if action == "sessions.next_host" or action == "sessions.previous_host" then
      local step = action == "sessions.next_host" and 1 or -1
      for offset = 1, #items do
        local index = (at - 1 + offset * step) % #items + 1
        if items[index].kind == "host" then
          cursor:select(index)
          break
        end
      end
      return true
    end

    if action == "sessions.next_attention" then
      local all = session_model.build(sessions(), "", false, true, "")
      local start = 0
      for index, item in ipairs(all) do
        if item.target == cursor:id() then
          start = index
          break
        end
      end
      for offset = 1, #all do
        local item = all[(start + offset - 1) % #all + 1]
        local status = item.session and item.session.status
        if
          status == "blocked"
          or status == "done"
          or status == "error"
          or status == "unreachable"
        then
          cursor:follow(item.target)
          store.selected = item.target
          return true
        end
      end
      command("message", { text = "no sessions need attention" })
      return true
    end

    if action == "sessions.collapse_host" or action == "sessions.expand_host" then
      local folding = action == "sessions.collapse_host"
      if selected and selected.kind == "repo" then
        set_repo_folded(selected.target, folding)
      else
        local host = selected and selected.host
        if host then
          if folding then
            cursor:select_by_id("host:" .. host)
          end
          set_host_folded(host, folding)
        end
      end
      return true
    end
    if selected and selected.kind ~= "session" then
      id = nil
    end

    -- Actions, not chords. The kernel already resolved which key was pressed,
    -- so the capital-vs-shift encoding trap is its problem now, not ours.
    if action == "sessions.open" then
      -- The agent pane is what shows a session; focusing it is what "open" means
      -- here, exactly as v1's Enter moves focus to the terminal.
      if id then
        command("focus", { text = "agent" })
      elseif group then
        return pane.on_action("sessions.toggle_host")
      end
    elseif action == "sessions.next" then
      cursor:move(1)
    elseif action == "sessions.previous" then
      cursor:move(-1)
    elseif action == "sessions.first" then
      cursor:select(1)
    elseif action == "sessions.last" then
      cursor:select(#items)
    elseif action == "sessions.page_up" or action == "sessions.page_down" then
      local step = action == "sessions.page_down" and 1 or -1
      cursor:select(math.max(1, math.min(#items, at + step * (state["sessions.page_size"] or 1))))

    -- Every state change below is a COMMAND: accepted instantly, its effect
    -- appearing in a later snapshot. Nothing here waits for anything.
    elseif action == "sessions.delete" and id then
      if soft_delete() then
        -- The shared confirmation records the undo target only on yes.
        local session = items[at].session
        store.confirm = {
          question = "Delete " .. (session.name or "this session") .. "?",
          lines = {},
          command = "delete",
          options = {
            session = id,
            remember = { key = "sessions.deleted", value = id },
          },
        }
      else
        -- The switch is off, so this key deletes for good.
        local session = items[at].session
        delete_for_good(session, "Delete " .. (session.name or "this session") .. " for good?")
      end
    elseif action == "sessions.force_delete" and id then
      -- Destructive, and undone by nothing. Risk lines explain what can be lost.
      local session = items[at].session
      delete_for_good(
        session,
        "Delete " .. (session.name or "this session") .. " and its worktree?"
      )
    elseif action == "sessions.restart" and id then
      local session = items[at].session
      store.confirm = {
        question = "Restart " .. (session.name or "this session") .. "?",
        lines = { "the current agent process will stop" },
        command = "restart",
        options = { session = id },
      }
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
    elseif action == "sessions.rename" and id then
      -- The field lives in the rename float, handed over through `store` as a
      -- question is handed to `confirm`: this pane knows which session and what
      -- it is called, and decides nothing about how the new name is asked for.
      store.rename = { session = id, name = items[at].session.name or "" }
    elseif action == "sessions.sync" and id then
      local session = items[at].session
      store.confirm = {
        question = "Sync " .. (session.name or "this session") .. "?",
        lines = { "its worktree will be updated" },
        command = "sync",
        options = { session = id },
      }
    elseif action == "sessions.editor" and id then
      command("editor", { session = id })
    -- A move is computed over the RENDERED items and sent whole, so a root row
    -- drags its subtree and a group edge moves the group. The cursor FOLLOWS the
    -- session rather than the row index, since the order it was pressed at lands
    -- a frame or two later.
    elseif action == "sessions.move_down" and id then
      local arranged = ordering_items()
      local position
      for index, item in ipairs(arranged) do
        if item.target == id then
          position = index
          break
        end
      end
      local moved = position and order.move_block(arranged, position, true)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.move_up" and id then
      local arranged = ordering_items()
      local position
      for index, item in ipairs(arranged) do
        if item.target == id then
          position = index
          break
        end
      end
      local moved = position and order.move_block(arranged, position, false)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.sort" then
      cursor:follow(id)
      persist_order(order.sorted_within_groups(ordering_items()))
    else
      return false
    end
    return true
  end,
}

return pane
