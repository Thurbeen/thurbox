-- Global search.
--
-- v1's is one strip that searches every scope at once and highlights the matches
-- **in the panes themselves** rather than reprinting them here. That is the
-- design decision worth preserving: results you can see in place are results you
-- can judge without leaving the list you were already reading — which is also
-- why this is a strip and not a float. A float would cover the rows it is
-- pointing at.
--
-- Two kinds of result, in two sections:
--
--   * **sessions** — a session whose name, agent, branch or repository matches.
--     Matched here, every frame, with `lib.fuzzy`, the same matcher the session
--     list lights its own rows with, so the two cannot disagree.
--   * **text** — a LINE in a session's terminals: everything its agent pane and
--     its shell still hold, scrollback included, not just what is on screen.
--     That is thousands of lines per session, so the kernel reads and matches it
--     on a worker (`kernel::search`) and this pane draws the answer a frame or
--     two later. The query is left in `store` under `want_content` once it has
--     stood still, and the answer arrives as `thurbox.search`.
--
-- Moving the selection PREVIEWS: the session list's cursor moves to the result,
-- and for a text hit the agent pane scrolls back to the line, while focus stays
-- here. `esc` puts back what you were looking at; `enter` keeps the jump and
-- lands you in the terminal, on the line.
--
-- The query language is `lib.fuzzy.query`'s: words (all must match, any order),
-- "quoted phrases", /regex/ (text only), `in:<session>` and `repo:<repo>`
-- filters, smart case. `tab` cycles what is searched: everything, text only,
-- or names only.

local fuzzy = require("lib.fuzzy")
local panels = require("lib.panels")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local NAME = "search"
local OPEN = "search.open"
local NEXT, PREVIOUS = "search.next", "search.previous"
local PAGE_DOWN, PAGE_UP = "search.page_down", "search.page_up"
local ACTIVATE, CANCEL = "search.activate", "search.cancel"
local SCOPE = "search.scope"

--- The live query, in `store` rather than in `state`.
---
--- It has to be readable from OUTSIDE this plugin: the session list highlights
--- its own rows against it, and `store` is the documented bus between panes.
--- Cleared on close, so a pane can treat "there is a query" and "search is
--- open" as the same question.
local QUERY = "search.query"

--- The sessions the current results name, space-separated in list order.
---
--- Published so the session list dims exactly the rows this strip did not
--- find — including a session found only by its terminal text, which the list
--- cannot know about by matching its own fields. A string, not a table: it is
--- written from render, and a string that did not change compares equal.
local MATCHES = "search.matches"

--- Where the kernel is asked for terminal text, and where the ask is narrowed
--- to some sessions (`kernel::search::WANT_CONTENT` / `WANT_SESSIONS`).
local WANT_CONTENT = "want_content"
local WANT_SESSIONS = "want_content.sessions"

--- The agent pane's scroll request, and the action that makes it read it.
local REVEAL = "terminal.reveal"

--- How long a query must stand still before terminals are searched. v1 waits
--- the same 150ms (`CONTENT_DEBOUNCE_MS`).
local CONTENT_DEBOUNCE = 0.15

--- Rows a page key moves through the results.
local PAGE = 10

--- What `tab` cycles through, and what the strip calls each.
local SCOPES = { "all", "text", "names" }
local SCOPE_LABEL = { all = "everything", text = "terminal text", names = "names" }

--- What each section is headed.
local SECTION = { sessions = "sessions", text = "text" }

-- ── State ───────────────────────────────────────────────────────────────────
--
-- Read whole, written whole: `state` hands back a fresh table on every read, so
-- a field mutated in place is silently lost.

local function load()
  return {
    field = state.field or textinput.new(""),
    cursor = state.cursor or 1,
    -- What the interface looked like when search opened, so cancelling can put
    -- it back. v1 captures the same things in its `SearchSnapshot`.
    snapshot = state.snapshot,
    -- The ask the debounce is timing, and when it was last seen to change.
    -- Measured in `ctx.elapsed` seconds: a plugin has no clock of its own.
    pending = state.pending,
    pending_at = state.pending_at,
    -- The result this pane last pointed the list at. Kept because the result set
    -- can change without a keystroke: terminal hits arrive a frame after the
    -- query settles, and a preview that only followed the arrows would stay on
    -- whatever was under the cursor before they appeared.
    previewed = state.previewed,
    -- The terminal a preview scrolled back, so the next preview or a cancel can
    -- put it back at the bottom. One at a time: a preview is a look, not a trail
    -- of scrolled terminals.
    revealed = state.revealed,
    scope = state.scope or "all",
  }
end

local function save(search)
  state.field = search.field
  state.cursor = search.cursor
  state.snapshot = search.snapshot
  state.pending = search.pending
  state.pending_at = search.pending_at
  state.previewed = search.previewed
  state.revealed = search.revealed
  state.scope = search.scope
end

local function query()
  return store[QUERY] or ""
end

-- ── Results ─────────────────────────────────────────────────────────────────

--- The fields of a session worth matching, name first: a row highlights its own
--- name when the name matched, and explains itself when something else did.
local function fields_of(session)
  return {
    { name = "name", text = session.name or "" },
    { name = "agent", text = session.agent or "" },
    { name = "branch", text = session.branch or "" },
    { name = "repo", text = session.repo or "" },
  }
end

--- The sessions the query's filters let through, in list order.
local function allowed(q)
  local list = {}
  for _, session in ipairs(thurbox.sessions or {}) do
    if fuzzy.passes(q, session) then
      list[#list + 1] = session
    end
  end
  return list
end

--- What the terminals should be asked for, or nil for nothing: the query's
--- terms, and the session ids to limit them to when a filter is in force.
local function ask_of(q, scope, sessions)
  if scope == "names" or q.empty then
    return nil
  end
  local within = nil
  if next(q.filters) then
    local ids = {}
    for _, session in ipairs(sessions) do
      ids[#ids + 1] = session.id
    end
    within = table.concat(ids, " ")
  end
  return { terms = q.terms, within = within }
end

--- The kernel's answer, if it answers the ask in force. An answer to an older
--- query is not shown with this one's name on it: its highlights would be
--- wrong and its hits would vanish a frame later.
local function answer_for(ask)
  local answer = thurbox and thurbox.search
  if not ask or not answer then
    return nil
  end
  if answer.query ~= ask.terms or answer.within ~= ask.within then
    return nil
  end
  return answer
end

--- Every result for the current query, and what the strip should say about it.
---
--- An empty query yields every session, which is what makes the strip useful
--- the moment it opens rather than only once you have typed something.
local function results(scope)
  local q = fuzzy.query(query())
  local sessions = allowed(q)
  local rows = {}

  if scope ~= "text" then
    local found = {}
    for order, session in ipairs(sessions) do
      local m = q.empty and { score = 0, positions = {} }
        or fuzzy.match_fields(q, fields_of(session))
      if m then
        found[#found + 1] = {
          scope = "sessions",
          id = session.id,
          session = session.id,
          label = session.name or session.id,
          positions = m.positions,
          -- Only the name is highlighted, so a hit anywhere else has to say so
          -- — otherwise the row looks like it matched nothing.
          snippet = m.field and (m.field .. ": " .. m.text) or nil,
          score = m.score,
          order = order,
        }
      end
    end
    -- Best first; the list's own order among equals, so an empty query reads
    -- exactly like the session list.
    table.sort(found, function(a, b)
      if a.score ~= b.score then
        return a.score > b.score
      end
      return a.order < b.order
    end)
    for _, row in ipairs(found) do
      rows[#rows + 1] = row
    end
  end

  local ask = ask_of(q, scope, sessions)
  local answer = answer_for(ask)
  if answer then
    local names = {}
    for _, session in ipairs(sessions) do
      names[session.id] = session.name or session.id
    end
    for index, hit in ipairs(answer.hits or {}) do
      -- A session deleted since the search ran has nowhere to land.
      if names[hit.session] then
        rows[#rows + 1] = {
          scope = "text",
          id = "hit:" .. index,
          session = hit.session,
          label = names[hit.session],
          hit = hit,
        }
      end
    end
  end

  return rows,
    {
      q = q,
      sessions = #sessions,
      ask = ask,
      answer = answer,
      searching = ask ~= nil and answer == nil,
    }
end

--- Scope counts, for the per-section headers.
local function counts(rows)
  local totals = {}
  for _, row in ipairs(rows) do
    totals[row.scope] = (totals[row.scope] or 0) + 1
  end
  return totals
end

--- The session ids the results name, in order, once each — for `MATCHES`.
local function matched_ids(rows)
  local seen, ids = {}, {}
  for _, row in ipairs(rows) do
    if not seen[row.session] then
      seen[row.session] = true
      ids[#ids + 1] = row.session
    end
  end
  return table.concat(ids, " ")
end

-- ── Preview and jump ────────────────────────────────────────────────────────

--- The surface a text hit is on: the session, or its `#shell`.
local function surface_of(hit)
  return hit.session .. (hit.shell and "#shell" or "")
end

--- Ask the agent pane to scroll: `"<surface> <offset>"` to show a line,
--- `"-<surface>"` to put it back at the bottom. Several, `;`-separated, in one
--- request, because the pane reads `store` when the action reaches it — after
--- this handler returns — so a second write would overwrite the first.
local function reveal(requests)
  if #requests == 0 then
    return
  end
  store[REVEAL] = table.concat(requests, ";")
  command("action", { text = REVEAL })
end

--- Point the owning pane at a result, without leaving the strip.
---
--- The session list follows a `store` selection it did not write itself, so
--- writing one moves its cursor while focus stays here. A text hit also scrolls
--- the terminal to the line when `scroll` is set — the keys do, a render does
--- not, because a render must not command the agent pane on every frame a hit
--- list moves under the cursor.
local function preview(search, row, scroll)
  if not row then
    return
  end
  panels.show("sessions")
  store.selected = row.session
  if not scroll then
    return
  end
  local requests = {}
  local surface = row.hit and surface_of(row.hit) or nil
  if search.revealed and search.revealed ~= surface then
    requests[#requests + 1] = "-" .. search.revealed
  end
  if row.hit then
    requests[#requests + 1] = surface .. " " .. math.floor(row.hit.scroll or 0)
  end
  search.revealed = surface
  reveal(requests)
end

--- Put back what was on screen before search opened.
---
--- Cancelling has to be a real undo, not just a close: previewing has already
--- moved a cursor and maybe scrolled a terminal, and leaving them where the last
--- preview put them would make `esc` a way to change both by accident.
local function restore(search)
  if search.revealed then
    reveal({ "-" .. search.revealed })
  end
  local snapshot = search.snapshot
  if not snapshot then
    return
  end
  if snapshot.selected then
    store.selected = snapshot.selected
  end
  if snapshot.sessions_shown ~= nil then
    store["panels.sessions"] = snapshot.sessions_shown
  end
end

--- Ask the kernel to search terminals once the ask has stood still.
---
--- Called from render because that is where the clock is: `ctx.elapsed` is the
--- only monotonic reading a plugin gets, and `on_key` has none. An ask still
--- settling asks for nothing, so terminals are searched once per pause rather
--- than once per keystroke.
local function want_content(search, ask, elapsed)
  if not ask then
    store[WANT_CONTENT] = nil
    store[WANT_SESSIONS] = nil
    search.pending, search.pending_at = nil, nil
    return
  end
  local key = ask.terms .. "\n" .. (ask.within or "*")
  if search.pending ~= key then
    search.pending, search.pending_at = key, elapsed
    return
  end
  if elapsed - (search.pending_at or elapsed) >= CONTENT_DEBOUNCE then
    store[WANT_CONTENT] = ask.terms
    store[WANT_SESSIONS] = ask.within
  end
end

local function close(search, keep)
  if not keep then
    restore(search)
  end
  search.snapshot = nil
  textinput.clear(search.field)
  search.cursor = 1
  search.pending, search.pending_at = nil, nil
  search.previewed = nil
  search.revealed = nil
  save(search)
  store[QUERY] = nil
  store[MATCHES] = nil
  -- Stop the kernel reading terminals the moment nothing is looking at them.
  store[WANT_CONTENT] = nil
  store[WANT_SESSIONS] = nil
  panels.hide(NAME)
end

--- Go to a result: keep the preview, close, and land in the terminal.
local function activate(search, row)
  preview(search, row, true)
  close(search, true)
  -- v1's Enter lands you IN the result: a session result focuses that
  -- session's terminal, not the row you picked it from.
  command("focus", { text = "agent" })
end

-- ── Rendering ───────────────────────────────────────────────────────────────

--- A count with thousands separated, so `12345 lines` reads at a glance.
local function thousands(n)
  local text = tostring(math.floor(n or 0))
  local out = text:reverse():gsub("(%d%d%d)", "%1,"):reverse()
  return (out:gsub("^,", ""))
end

local function query_row(search, rows, info)
  local total = #rows
  local position = total > 0 and math.min(search.cursor, total) or 0
  local tail
  if total > 0 then
    tail = "[" .. position .. "/" .. total .. "]"
  elseif info.searching then
    tail = "searching…"
  else
    tail = "no matches"
  end
  local label = search.scope == "all" and " Search " or (" Search " .. search.scope .. " ")
  return {
    type = "box",
    axis = "horizontal",
    len = 1,
    children = {
      {
        type = "text",
        len = widgets.len(label),
        text = { { { text = label, style = { fg = theme.accent, bold = true } } } },
      },
      {
        type = "input",
        fill = 1,
        value = search.field.value or "",
        cursor = search.field.cursor or 0,
        placeholder = 'words, "a phrase", /regex/, in:session, repo:name',
        -- The strip is only drawn while searching, and while it is the query is
        -- what the keyboard is aimed at.
        focused = true,
        style = { fg = theme.text },
      },
      {
        type = "text",
        len = widgets.len(tail) + 1,
        text = { { { text = tail .. " ", style = { fg = theme.muted } } } },
      },
    },
  }
end

--- One line under the query saying what was searched and how it went — the
--- answer to "why is my text not here" before anyone has to ask.
local function status_line(search, rows, info)
  local parts = {}
  local totals = counts(rows)
  if search.scope ~= "text" then
    parts[#parts + 1] = "names " .. (totals.sessions or 0)
  end
  if search.scope ~= "names" then
    local answer = info.answer
    if info.q.empty then
      parts[#parts + 1] = "type to search terminal text"
    elseif info.searching then
      parts[#parts + 1] = "searching " .. info.sessions .. " sessions' scrollback…"
    elseif answer and answer.error then
      parts[#parts + 1] = answer.error
    elseif answer then
      local shown = totals.text or 0
      local found = answer.total or shown
      parts[#parts + 1] = "text "
        .. shown
        .. (found > shown and (" of " .. thousands(found)) or "")
        .. " in "
        .. thousands(answer.lines)
        .. " lines of "
        .. answer.sessions
        .. " sessions ("
        .. math.floor((answer.ms or 0) + 0.5)
        .. "ms)"
    end
  end
  parts[#parts + 1] = "tab: " .. SCOPE_LABEL[search.scope]
  return {
    type = "text",
    len = 1,
    text = { { { text = " " .. table.concat(parts, " · "), style = { fg = theme.muted } } } },
  }
end

--- A session result: the name with its matched characters lit, and the field
--- that matched when it was not the name.
local function session_line(row, selected, width)
  local base = { fg = selected and theme.text or theme.secondary }
  local hit = { fg = theme.accent, bold = true, underline = true }
  local spans = fuzzy.spans(row.label, row.positions, base, hit)
  if row.snippet then
    local used = widgets.len(row.label) + 2
    local room = math.max(0, width - used - 3)
    if room > 4 then
      spans[#spans + 1] = {
        text = "  " .. widgets.truncate(row.snippet, room),
        style = { fg = theme.muted },
      }
    end
  end
  return { spans = spans, id = row.id }
end

--- Where a text hit sits: `on screen`, or how many rows back.
local function position_of(hit)
  local back = hit.back or 0
  if (hit.scroll or 0) == 0 then
    return "on screen"
  end
  return back .. "↑"
end

--- A text result: session, how far back, and the line with the match lit.
---
--- Columns shrink before the line does: under 40 columns the session name goes,
--- because the line is the thing being searched for and the list already
--- previews which session it is in.
local function text_line(row, selected, width)
  local hit = row.hit
  local base = { fg = selected and theme.text or theme.secondary }
  local lit = { fg = theme.accent, bold = true, underline = true }
  local spans = {}
  if width >= 40 then
    local name = row.label .. (hit.shell and " ·sh" or "")
    local column = math.min(18, math.max(8, math.floor(width / 5)))
    spans[#spans + 1] = {
      text = widgets.pad(widgets.truncate(name, column), column) .. " ",
      style = { fg = theme.muted },
    }
  end
  local where = position_of(hit)
  spans[#spans + 1] = {
    text = string.rep(" ", math.max(0, 9 - widgets.len(where))) .. where .. "  ",
    style = { fg = theme.muted },
  }
  for _, span in ipairs(fuzzy.spans(hit.text or "", hit.positions, base, lit)) do
    spans[#spans + 1] = span
  end
  return { spans = spans, id = row.id }
end

--- Results as lines, with a header per section, plus the LINE the cursor's
--- result sits on.
---
--- Selection is in RESULT space, because a header is not a thing you can pick,
--- so the mapping is returned rather than recomputed by the caller.
local function result_rows(rows, cursor, width, height, info, scope)
  -- Line numbers are assigned arithmetically first, then spans are built only
  -- for the visible window: every result used to be fully rendered for a strip
  -- that shows a handful. The list's own window is a sub-range of this one over
  -- the same (count, height, selected), so every line it draws has spans; an
  -- off-window entry is a placeholder it never reads.
  local entries, selected_line = {}, 1
  local totals = counts(rows)
  local section = nil
  for index, row in ipairs(rows) do
    if row.scope ~= section then
      section = row.scope
      entries[#entries + 1] = { header = SECTION[section] .. " " .. totals[section], key = section }
    end
    entries[#entries + 1] = { row = row, index = index }
    if index == cursor then
      selected_line = #entries
    end
  end
  -- While the terminals are being read, say so where their hits will appear,
  -- so a strip with only name matches is not mistaken for the whole answer.
  if info.searching and scope ~= "names" and #rows > 0 then
    entries[#entries + 1] = { header = "text: searching…", key = "searching" }
  end

  local first, last = widgets.window(#entries, height, selected_line)
  local lines = {}
  for at, entry in ipairs(entries) do
    if at < first or at > last then
      lines[at] = { spans = {}, id = entry.header and ("header:" .. entry.key) or entry.row.id }
    elseif entry.header then
      lines[at] = {
        spans = { { text = entry.header, style = { fg = theme.muted } } },
        -- Not addressable: a click on a header must not resolve to a result.
        id = "header:" .. entry.key,
      }
    elseif entry.row.hit then
      lines[at] = text_line(entry.row, entry.index == cursor, width)
    else
      lines[at] = session_line(entry.row, entry.index == cursor, width)
    end
  end
  return lines, selected_line
end

--- What an empty list says: what was searched, so "nothing" is an answer.
local function empty_text(search, info)
  if info.sessions == 0 then
    return "  no session passes the filter"
  end
  if info.searching then
    return "  searching " .. info.sessions .. " sessions…"
  end
  local what = search.scope == "names" and "names"
    or search.scope == "text" and "terminal text"
    or "names or terminal text"
  local lines = info.answer and (" (" .. thousands(info.answer.lines) .. " lines)") or ""
  return "  no match for "
    .. info.q.raw
    .. " in the "
    .. what
    .. " of "
    .. info.sessions
    .. " sessions"
    .. lines
end

--- Move the cursor by `step` results and preview where it lands.
local function step_cursor(step)
  local search = load()
  local rows = results(search.scope)
  search.cursor = widgets.clamp(search.cursor + step, #rows)
  local row = rows[search.cursor]
  search.previewed = row and row.id or nil
  preview(search, row, true)
  save(search)
end

return {
  name = NAME,
  slot = NAME,
  order = 65,
  -- Deliberately NOT `pure`. This render writes to `store` — the debounced
  -- `want_content` request and the matched-sessions list — so a frame that
  -- skipped it would skip those writes too, and the search would stop asking
  -- for the terminal text it matches against.
  -- Focusable, and focused while it is open: that is what routes every typed
  -- character here instead of to the pane underneath. A float would grab input
  -- instead, but a float would also cover the matches — see the header.
  focusable = true,

  keys = {
    -- v1's chord. The kernel folds the three encodings terminals deliver it as
    -- (`ctrl+/`, `ctrl+7`, `ctrl+_`) into this one, so declaring it once is
    -- enough. It is not a bare `ctrl+<letter>`, so it reaches search even from a
    -- focused agent.
    {
      key = "ctrl+/",
      action = OPEN,
      desc = "search sessions and their terminal text",
      scope = "global",
      group = "UI",
    },
    -- Arrows and page keys, deliberately NOT `j`/`k` or `ctrl+n`/`ctrl+p`:
    -- letters are what a search box types, and in v2 every chord goes through
    -- one registry where a plugin-scoped claim does not outrank a global one —
    -- declaring `ctrl+n` here would take it from new-session everywhere.
    { key = "down", action = NEXT, desc = "next result (previews it)", group = "Search" },
    { key = "up", action = PREVIOUS, desc = "previous result (previews it)", group = "Search" },
    { key = "pagedown", action = PAGE_DOWN, desc = "results, a page down", group = "Search" },
    { key = "pageup", action = PAGE_UP, desc = "results, a page up", group = "Search" },
    {
      key = "enter",
      action = ACTIVATE,
      desc = "open the result, scrolled to it",
      group = "Search",
    },
    { key = "tab", action = SCOPE, desc = "search everything / text / names", group = "Search" },
    { key = "esc", action = CANCEL, desc = "close and put back", group = "Search" },
  },

  render = function(ctx)
    local width, height = ctx.width or 0, ctx.height or 0
    local search = load()
    local rows, info = results(search.scope)
    want_content(search, info.ask, ctx.elapsed or 0)
    search.cursor = widgets.clamp(search.cursor, #rows)

    local matches = matched_ids(rows)
    if store[MATCHES] ~= matches then
      store[MATCHES] = matches
    end

    -- Point the list at whatever is under the cursor now. Only when the answer
    -- CHANGED: a frame that previews the row it previewed last time would fight
    -- the list for its own cursor.
    local current = rows[search.cursor]
    if current and current.id ~= search.previewed then
      preview(search, current, false)
      search.previewed = current.id
    end
    save(search)

    local children = {
      query_row(search, rows, info),
      status_line(search, rows, info),
    }

    -- Rows the list will actually be given: the strip, less the panel's two
    -- borders, less every sibling already above it. Counted rather than written
    -- out, because a sibling added later would otherwise quietly take a row back
    -- from the list -- and asking for one row more than the rect holds does not
    -- clip. An over-subscribed box hands the whole rect to its FIRST child and
    -- nothing to the others, so one line paints and every result disappears.
    local consumed = 2
    for _, child in ipairs(children) do
      consumed = consumed + (child.len or 0)
    end

    local list_height = math.max(0, height - consumed)
    local lines, selected_line =
      result_rows(rows, search.cursor, math.max(0, width - 4), list_height, info, search.scope)
    children[#children + 1] = widgets.list({
      rows = lines,
      selected = selected_line,
      height = list_height,
      fill = 1,
      empty = empty_text(search, info),
    })

    return {
      type = "box",
      frame = widgets.panel("Search", ctx.focused),
      children = children,
    }
  end,

  -- A click opens the result, the mouse's `enter`.
  on_click = function(hit)
    if not hit.id then
      return false
    end
    local search = load()
    local rows = results(search.scope)
    local index = widgets.index_of(rows, hit.id)
    if not index then
      return false
    end
    search.cursor = index
    activate(search, rows[index])
    return true
  end,

  on_action = function(action)
    if action == OPEN then
      -- A second press closes it, as every other panel key does. Cancelling
      -- rather than keeping, since nothing was chosen.
      if panels.shown(NAME) then
        close(load(), false)
        command("focus", { text = "sessions" })
        return true
      end
      local search = load()
      textinput.clear(search.field)
      search.cursor = 1
      search.revealed = nil
      -- Captured BEFORE anything is previewed, which is the only moment the
      -- pre-search selection is still readable.
      search.snapshot = {
        selected = store.selected,
        sessions_shown = panels.shown("sessions"),
      }
      store[QUERY] = ""
      panels.show(NAME)
      -- Preview at once, so the first `down` steps to the SECOND result rather
      -- than to the second while the list still shows the first. The snapshot
      -- above is what makes that safe to do before anything was chosen.
      local first = results(search.scope)[1]
      preview(search, first, false)
      search.previewed = first and first.id or nil
      save(search)
      command("focus", { text = NAME })
      return true
    end

    if not panels.shown(NAME) then
      return false
    end

    if action == NEXT or action == PREVIOUS then
      step_cursor(action == NEXT and 1 or -1)
      return true
    elseif action == PAGE_DOWN or action == PAGE_UP then
      step_cursor(action == PAGE_DOWN and PAGE or -PAGE)
      return true
    elseif action == SCOPE then
      local search = load()
      for index, scope in ipairs(SCOPES) do
        if scope == search.scope then
          search.scope = SCOPES[index % #SCOPES + 1]
          break
        end
      end
      search.cursor = 1
      save(search)
      return true
    elseif action == ACTIVATE then
      local search = load()
      local rows = results(search.scope)
      local row = rows[widgets.clamp(search.cursor, #rows)]
      if row then
        activate(search, row)
      end
      return true
    elseif action == CANCEL then
      close(load(), false)
      command("focus", { text = "sessions" })
      return true
    end
    return false
  end,

  --- Everything the declared keys did not take is typing.
  ---
  --- Returning true for any consumed key is what keeps a letter from reaching
  --- the pane underneath while the strip has focus.
  on_key = function(key)
    if not panels.shown(NAME) then
      return false
    end
    local search = load()
    if not textinput.key(search.field, key) then
      return false
    end
    -- A new query starts at the top of its own results.
    search.cursor = 1
    save(search)
    store[QUERY] = search.field.value or ""
    -- The preview follows the query as well as the arrows, from `render`, on
    -- the frame this keystroke causes: it clamps the cursor to the new results
    -- and previews whatever that lands on.
    return true
  end,
}
